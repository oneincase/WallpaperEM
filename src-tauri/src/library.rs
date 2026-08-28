//! 本地库（T4）：列表/删除/打开目录/Web 数据导入 + 应用到桌面

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::wallpaper;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryItem {
    pub item_id: String,
    pub title: String,
    pub r#type: String,
    pub preview_url: Option<String>,
    pub tags: Vec<String>,
    pub size_bytes: i64,
    pub file_count: i64,
    pub downloaded_at: i64,
}

#[tauri::command]
pub fn library_list(
    app: AppHandle,
    r#type: Option<String>,
) -> Result<Vec<LibraryItem>, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let mut sql = String::from(
        "SELECT l.item_id, l.title, l.type, COALESCE(w.preview_url, ''), COALESCE(w.tags,'[]'),
                l.size_bytes, l.file_count, l.downloaded_at, COALESCE(w.type, ''), COALESCE(l.tags,'[]')
         FROM library_items l LEFT JOIN workshop_items w ON w.id = l.item_id",
    );
    if let Some(t) = &r#type {
        if !t.is_empty() {
            sql.push_str(" WHERE l.type = '");
            sql.push_str(&t.replace('\'', "''"));
            sql.push('\'');
        }
    }
    sql.push_str(" ORDER BY l.downloaded_at DESC");
    let items: Vec<(LibraryItem, Option<(String, String)>)> = {
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                let mut tags: Vec<String> =
                    serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default();
                let ltags: Vec<String> =
                    serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default();
                for t in ltags {
                    if !tags.contains(&t) {
                        tags.push(t);
                    }
                }
                let item_id: String = r.get(0)?;
                let lt: String = r.get(2)?;
                let wt: String = r.get(8)?;
                // 本地 type 为 unknown 但工坊元数据有正确类型时：修正（含显示 + 写回 DB）
                let fix = if lt == "unknown" && !wt.is_empty() && wt != "unknown" {
                    Some((item_id.clone(), wt.clone()))
                } else {
                    None
                };
                Ok((
                    LibraryItem {
                        item_id,
                        title: r.get(1)?,
                        r#type: fix.as_ref().map(|(_, t)| t.clone()).unwrap_or(lt),
                        preview_url: {
                            let p: String = r.get(3)?;
                            if p.is_empty() {
                                None
                            } else {
                                Some(p)
                            }
                            },
                        tags,
                        size_bytes: r.get(5)?,
                        file_count: r.get(6)?,
                        downloaded_at: r.get(7)?,
                    },
                    fix,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };

    // 将已修正的 unknown 类型写回 library_items（一劳永逸，保证类型筛选也正确）
    if !items.is_empty() {
        for (_, fix) in items.iter() {
            if let Some((item_id, t)) = fix {
                let _ = conn.execute(
                    "UPDATE library_items SET type = ?1 WHERE item_id = ?2",
                    rusqlite::params![t, item_id],
                );
            }
        }
    }
    // 工作友好（默认开启）：本地库同样过滤成人 / Mature 内容
    let family_friendly = crate::sfw::is_family_friendly(&conn);
    drop(conn);

    let mut items = items.into_iter().map(|(it, _)| it).collect::<Vec<_>>();

    if family_friendly {
        items.retain(|it| !crate::sfw::is_adult_item(&it.title, &it.tags));
    }

    // 无工坊元数据的条目：回退到本地 preview.gif（经内容服务器）
    for it in items.iter_mut() {
        if it.preview_url.is_some() {
            continue;
        }
        if let Some(url) = local_preview_url(&app, &it.item_id) {
            it.preview_url = Some(url);
        }
    }
    Ok(items)
}

/// 壁纸目录内 preview.gif → 内容服务器 URL
fn local_preview_url(app: &AppHandle, item_id: &str) -> Option<String> {
    let port = app.try_state::<Arc<Mutex<u16>>>()?.lock().ok().map(|g| *g)?;
    if port == 0 {
        return None;
    }
    let dir = app
        .path()
        .app_data_dir()
        .ok()?
        .join("wallpapers")
        .join(item_id);
    let has_preview = dir.join("preview.gif").is_file()
        || dir.join("preview.png").is_file()
        || dir.join("preview.jpg").is_file();
    if !has_preview {
        return None;
    }
    let token = app.try_state::<crate::content_server::ContentServerState>()?.token.clone();
    let ext = if dir.join("preview.png").is_file() {
        "png"
    } else if dir.join("preview.jpg").is_file() {
        "jpg"
    } else {
        "gif"
    };
    Some(format!("http://127.0.0.1:{port}/media/{token}/{item_id}/preview.{ext}"))
}

#[tauri::command]
pub fn library_delete(app: AppHandle, item_id: String) -> Result<bool, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("wallpapers")
        .join(&item_id);

    // 只有「删除的是当前正应用的壁纸」时才停止对应屏幕；否则不要动壁纸引擎，
    // 避免删一个无关壁纸导致正在应用的壁纸消失。
    let applied_displays: Vec<String> = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT display_id FROM wallpaper_sessions WHERE item_id = ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([&item_id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    // 释放锁后再停止（stop 内部要跑主线程，避免持锁等待）
    for d in applied_displays {
        let _ = wallpaper::stop(app.clone(), Some(d));
    }

    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删除文件失败: {e}"))?;
    }
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM library_items WHERE item_id = ?1", [&item_id])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM wallpaper_sessions WHERE item_id = ?1", [&item_id])
        .map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
pub fn library_open_folder(app: AppHandle, item_id: String) -> Result<bool, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("wallpapers")
        .join(&item_id);
    if !dir.is_dir() {
        return Err("目录不存在".into());
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(dir.display().to_string(), None::<&str>)
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// 一键导入 Web 版数据（壁纸文件 + 工坊元数据）
#[tauri::command]
pub fn library_import_from_web(app: AppHandle, web_data_dir: String) -> Result<serde_json::Value, String> {
    let src = Path::new(&web_data_dir);
    if !src.is_dir() {
        return Err(format!("目录不存在: {web_data_dir}"));
    }
    let wallpapers_src = src.join("wallpapers");
    if !wallpapers_src.is_dir() {
        return Err("未找到 wallpapers/ 子目录（请选择 Web 版 apps/server/data 目录）".into());
    }
    let db = app.state::<Arc<Mutex<Connection>>>();
    let dest_root = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("wallpapers");
    std::fs::create_dir_all(&dest_root).map_err(|e| e.to_string())?;

    let mut imported = 0usize;
    let mut skipped = 0usize;
    for entry in std::fs::read_dir(&wallpapers_src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let item_id = entry.file_name().to_string_lossy().to_string();
        let dest = dest_root.join(&item_id);
        if !dest.is_dir() {
            let _ = std::fs::remove_dir_all(&dest);
            copy_dir(&entry.path(), &dest)?;
        } else {
            skipped += 1;
        }
        // 始终重新解析 project.json 并 upsert（修正类型）
        let (wtype, title) = parse_project(&dest);
        {
            let conn = db.lock().map_err(|e| e.to_string())?;
            let _ = conn.execute(
                "INSERT INTO library_items(item_id, title, type, size_bytes, file_count, downloaded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())
                 ON CONFLICT(item_id) DO UPDATE SET title = ?2, type = ?3, size_bytes = ?4, file_count = ?5",
                rusqlite::params![item_id, title, wtype, dir_size(&dest), dir_count(&dest)],
            );
        }
        imported += 1;
    }
    Ok(json!({ "imported": imported, "skipped": skipped }))
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = to.join(entry.file_name());
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn dir_size(dir: &Path) -> i64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                total += dir_size(&e.path()) as u64;
            } else {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total as i64
}

fn dir_count(dir: &Path) -> i64 {
    let mut n = 0i64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                n += dir_count(&e.path());
            } else {
                n += 1;
            }
        }
    }
    n
}

fn parse_project(dir: &Path) -> (String, String) {
    let project = dir.join("project.json");
    if let Ok(text) = std::fs::read_to_string(&project) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            let t = v
                .get("type")
                .and_then(|t| t.as_str())
                .map(|s| crate::steam::details::infer_type_from_tags(&[s.to_string()]))
                .unwrap_or_else(|| "unknown".into());
            let title = v
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or_else(|| "未命名".into())
                .to_string();
            return (t, title);
        }
    }
    ("unknown".into(), "未命名".into())
}
