//! 本地库（T4）：列表/删除/打开目录/Web 数据导入 + 应用到桌面
//!
//! 另含 WE 网页壁纸用户属性命令（读取/保存/重置，保存后对已应用窗口热更新）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::wallpaper;
use crate::we_props;

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
    /// 磁盘上的壁纸文件已丢失（目录不存在或为空），条目仅剩数据库记录。
    ///
    /// 刻意只标记、不自动删记录：删除不可撤销，且会连带丢掉用户的属性覆盖；
    /// 真正的清理由用户在本地库页显式触发（`library_prune`）。
    pub missing: bool,
}

/// 本地库筛选参数。全部可选，未提供即不约束该维度。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryFilter {
    /// 壁纸类型（video/scene/web/...），空串或 None = 不过滤
    #[serde(default)]
    pub r#type: Option<String>,
    /// 标题模糊搜索
    #[serde(default)]
    pub query: Option<String>,
    /// 必需标签（AND，语义与工坊一致）
    #[serde(default)]
    pub tags: Vec<String>,
    /// 排除标签（命中任一即排除）
    #[serde(default)]
    pub excluded_tags: Vec<String>,
    /// 文件体积范围（字节）
    #[serde(default)]
    pub min_size: Option<i64>,
    #[serde(default)]
    pub max_size: Option<i64>,
    /// 下载时间范围（unix 秒）
    #[serde(default)]
    pub downloaded_after: Option<i64>,
    #[serde(default)]
    pub downloaded_before: Option<i64>,
    /// 只看文件已丢失的条目
    #[serde(default)]
    pub only_missing: bool,
    /// 排序键，见 `order_by_clause` 的白名单
    #[serde(default)]
    pub sort: Option<String>,
}

/// 排序键 → SQL ORDER BY。**白名单映射**，绝不把用户输入拼进 SQL。
fn order_by_clause(sort: Option<&str>) -> &'static str {
    match sort.unwrap_or("downloaded_desc") {
        "downloaded_asc" => "ORDER BY l.downloaded_at ASC",
        "title_asc" => "ORDER BY l.title COLLATE NOCASE ASC",
        "title_desc" => "ORDER BY l.title COLLATE NOCASE DESC",
        "size_desc" => "ORDER BY l.size_bytes DESC",
        "size_asc" => "ORDER BY l.size_bytes ASC",
        _ => "ORDER BY l.downloaded_at DESC",
    }
}

#[tauri::command]
pub fn library_list(
    app: AppHandle,
    r#type: Option<String>,
    filter: Option<LibraryFilter>,
) -> Result<Vec<LibraryItem>, String> {
    // 兼容旧签名：只传 type 时等价于一个仅含类型的筛选
    let mut f = filter.unwrap_or_default();
    if f.r#type.is_none() {
        f.r#type = r#type;
    }

    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;

    // 全部条件走参数绑定。原实现把 type 字符串直接拼进 SQL，加了标题搜索这类
    // 自由文本后就是注入面，这里一次性改掉。
    let mut where_parts: Vec<String> = Vec::new();
    let mut binds: Vec<rusqlite::types::Value> = Vec::new();
    use rusqlite::types::Value;

    if let Some(t) = f.r#type.as_deref().filter(|t| !t.is_empty()) {
        where_parts.push("l.type = ?".into());
        binds.push(Value::Text(t.to_string()));
    }
    if let Some(q) = f.query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        where_parts.push("l.title LIKE ? ESCAPE '\\'".into());
        // 用户输入里的 LIKE 通配符要转义，否则搜 "100%" 会匹配到一切
        let esc = q
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        binds.push(Value::Text(format!("%{esc}%")));
    }
    if let Some(v) = f.min_size {
        where_parts.push("l.size_bytes >= ?".into());
        binds.push(Value::Integer(v));
    }
    if let Some(v) = f.max_size {
        where_parts.push("l.size_bytes <= ?".into());
        binds.push(Value::Integer(v));
    }
    if let Some(v) = f.downloaded_after {
        where_parts.push("l.downloaded_at >= ?".into());
        binds.push(Value::Integer(v));
    }
    if let Some(v) = f.downloaded_before {
        where_parts.push("l.downloaded_at <= ?".into());
        binds.push(Value::Integer(v));
    }
    // 标签：本地 tags 与工坊 tags 取并集后判断。用 json_each 展开，
    // 空/坏 JSON 由 json_valid 兜底（否则整条 SQL 会因解析失败报错）。
    let tag_exists = "EXISTS (SELECT 1 FROM json_each(
            CASE WHEN json_valid(l.tags) THEN l.tags ELSE '[]' END
         ) WHERE value = ?
       ) OR EXISTS (SELECT 1 FROM json_each(
            CASE WHEN json_valid(w.tags) THEN w.tags ELSE '[]' END
         ) WHERE value = ?
       )";
    for t in f.tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        where_parts.push(format!("({tag_exists})"));
        binds.push(Value::Text(t.to_string()));
        binds.push(Value::Text(t.to_string()));
    }
    for t in f
        .excluded_tags
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
    {
        where_parts.push(format!("NOT ({tag_exists})"));
        binds.push(Value::Text(t.to_string()));
        binds.push(Value::Text(t.to_string()));
    }

    let mut sql = String::from(
        "SELECT l.item_id, l.title, l.type, COALESCE(w.preview_url, ''), COALESCE(w.tags,'[]'),
                l.size_bytes, l.file_count, l.downloaded_at, COALESCE(w.type, ''), COALESCE(l.tags,'[]')
         FROM library_items l LEFT JOIN workshop_items w ON w.id = l.item_id",
    );
    if !where_parts.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_parts.join(" AND "));
    }
    sql.push(' ');
    sql.push_str(order_by_clause(f.sort.as_deref()));

    let items: Vec<(LibraryItem, Option<(String, String)>)> = {
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
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
                        // 占位，出 SQL 作用域后统一按磁盘实际情况回填
                        missing: false,
                    },
                    fix,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
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
    let mut items = items.into_iter().map(|(it, _)| it).collect::<Vec<_>>();

    // 与磁盘对账：标记文件已丢失的条目（不在此处删记录，理由见 LibraryItem::missing）。
    //
    // 安全闸：只有当壁纸库根目录本身可读时才逐条判定。根目录不存在/读不了
    // （首次启动、数据目录被清空、TCC 权限未授予）时全部按「存在」处理 ——
    // 否则会把整个库一次性标成失效，用户一点「清理」就全没了。
    let root_ok = wallpapers_dir(&app).map(|d| d.is_dir()).unwrap_or(false);
    if root_ok {
        for it in items.iter_mut() {
            if let Ok(dir) = item_dir(&app, &it.item_id) {
                it.missing = !item_files_exist(&dir);
            }
        }
    }
    // 「只看失效条目」只能在对账之后过滤 —— missing 不是数据库里的列，
    // 是刚刚按磁盘实际情况算出来的
    if f.only_missing {
        items.retain(|it| it.missing);
    }

    // 无工坊元数据的条目：回退到本地 preview.*（经内容服务器）
    for it in items.iter_mut() {
        if it.preview_url.is_some() {
            continue;
        }
        if let Some(url) = local_preview_url(&app, &it.item_id) {
            it.preview_url = Some(url);
            continue;
        }
        // 历史导入（本功能上线前）的视频没有 preview.*：惰性抽首帧补齐封面。
        // 一次成型 —— 生成后下一轮列表直接命中 preview.png，不再走这里。
        // 抽帧失败（mkv/avi 等系统解不了的格式）只记日志，条目保持无封面。
        if it.r#type == "video" {
            if let Ok(dir) = item_dir(&app, &it.item_id) {
                if let Some(video) = first_video_file(&dir) {
                    let out = dir.join("preview.png");
                    match crate::system_wallpaper::video_poster_png(&video, &out) {
                        Ok(()) => {
                            it.preview_url = local_preview_url(&app, &it.item_id);
                        }
                        Err(e) => {
                            tracing::warn!("视频封面惰性抽帧失败（{}）: {e}", it.item_id);
                        }
                    }
                }
            }
        }
    }
    Ok(items)
}

/// 壁纸目录内 preview.gif → 内容服务器 URL
fn local_preview_url(app: &AppHandle, item_id: &str) -> Option<String> {
    let port = app
        .try_state::<Arc<Mutex<u16>>>()?
        .lock()
        .ok()
        .map(|g| *g)?;
    if port == 0 {
        return None;
    }
    let dir = app
        .path()
        .app_data_dir()
        .ok()?
        .join("wallpapers")
        .join(item_id);
    // 与导入侧的 preview.<ext> 命名一一对应；优先级固定，与目录枚举顺序无关
    let ext = ["gif", "png", "jpg", "webp"]
        .iter()
        .find(|e| dir.join(format!("preview.{e}")).is_file())?;
    let token = app
        .try_state::<crate::content_server::ContentServerState>()?
        .token
        .clone();
    Some(format!(
        "http://127.0.0.1:{port}/media/{token}/{item_id}/preview.{ext}"
    ))
}

/// 清除一个 item_id 在数据库里的全部痕迹（不碰磁盘）。
///
/// `library_delete` 与 `library_prune` 共用，保证「删壁纸」在两条路径上语义一致。
/// 刻意**不动**这两张表：
/// - `workshop_items`：工坊元数据缓存，与本地有没有文件正交；删了工坊页会丢标题和预览图
/// - `favorites`：收藏是用户意图，不该因为文件没了就被替用户取消
fn purge_item_records(conn: &Connection, item_id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM library_items WHERE item_id = ?1", [item_id])
        .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM wallpaper_sessions WHERE item_id = ?1",
        [item_id],
    )
    .map_err(|e| e.to_string())?;
    // 用户属性覆盖：不清的话，删掉再重新下载同一个壁纸会「复活」上次的配置
    let _ = conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        [format!("web_props:{item_id}")],
    );
    // 已结束的下载历史（进行中的任务不动，避免打断正在跑的队列）
    let _ = conn.execute(
        "DELETE FROM downloads WHERE item_id = ?1 AND status IN ('done','failed')",
        [item_id],
    );
    remove_from_playlists(conn, item_id);
    Ok(())
}

/// 把 item_id 从所有播放列表里摘掉。
///
/// item_ids 是 JSON 数组，只能读出来过滤再写回。`settings.active_playlist` 存的是
/// 整个播放列表的 JSON 快照（与 playlists 表是两份独立数据），必须一起改，
/// 否则轮播每轮仍会在这个已失效的条目上空转一次。
fn remove_from_playlists(conn: &Connection, item_id: &str) {
    let rows: Vec<(i64, String)> = {
        let Ok(mut stmt) = conn.prepare("SELECT id, item_ids FROM playlists") else {
            return;
        };
        let Ok(mapped) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        else {
            return;
        };
        mapped.filter_map(|r| r.ok()).collect()
    };
    for (id, raw) in rows {
        let Ok(ids) = serde_json::from_str::<Vec<String>>(&raw) else {
            continue;
        };
        if !ids.iter().any(|i| i == item_id) {
            continue;
        }
        let kept: Vec<String> = ids.into_iter().filter(|i| i != item_id).collect();
        let next = serde_json::to_string(&kept).unwrap_or_else(|_| "[]".into());
        let _ = conn.execute(
            "UPDATE playlists SET item_ids = ?1 WHERE id = ?2",
            rusqlite::params![next, id],
        );
    }
    // 当前激活的播放列表快照
    if let Some(raw) = crate::db::get_setting(conn, "active_playlist") {
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(arr) = v.get_mut("itemIds").and_then(|x| x.as_array_mut()) {
                let before = arr.len();
                arr.retain(|x| x.as_str() != Some(item_id));
                if arr.len() != before {
                    let _ = crate::db::set_setting(conn, "active_playlist", &v.to_string());
                }
            }
        }
    }
}

#[tauri::command]
pub fn library_delete(app: AppHandle, item_id: String) -> Result<bool, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let dir = item_dir(&app, &item_id)?;

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
    purge_item_records(&conn, &item_id)?;
    Ok(true)
}

/// 与磁盘对账：清理「数据库有记录但壁纸文件已不存在」的条目。
///
/// 由用户在本地库页显式触发。返回被清理的条目 id 列表。
#[tauri::command]
pub fn library_prune(app: AppHandle) -> Result<Vec<String>, String> {
    let root = wallpapers_dir(&app)?;
    // 安全闸：根目录读不到时一律不清理。数据目录未就绪/权限缺失的情况下
    // 逐条判定会把整个库判成失效，这是不可撤销的数据丢失。
    if !root.is_dir() {
        return Err("壁纸库目录不可访问，已取消清理（避免误删记录）".into());
    }

    let db = app.state::<Arc<Mutex<Connection>>>();
    let ids: Vec<String> = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT item_id FROM library_items")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    };

    let missing: Vec<String> = ids
        .into_iter()
        .filter(|id| !item_files_exist(&root.join(id)))
        .collect();
    if missing.is_empty() {
        return Ok(Vec::new());
    }

    // 失效壁纸可能还挂在某块屏幕上（会话记录指向它），先停掉再删记录
    let stale_displays: Vec<String> = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for id in &missing {
            if let Ok(mut stmt) =
                conn.prepare("SELECT display_id FROM wallpaper_sessions WHERE item_id = ?1")
            {
                if let Ok(rows) = stmt.query_map([id], |r| r.get::<_, String>(0)) {
                    out.extend(rows.filter_map(|r| r.ok()));
                }
            }
        }
        out
    };
    for d in stale_displays {
        let _ = wallpaper::stop(app.clone(), Some(d));
    }

    let conn = db.lock().map_err(|e| e.to_string())?;
    for id in &missing {
        purge_item_records(&conn, id)?;
        tracing::info!("library prune: 清理失效条目 {id}（壁纸文件已不存在）");
    }
    Ok(missing)
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

// ---------- 导入：自定义文件 / Web 版数据 ----------
//
// 设计原则（这里修过的坑，别再改回去）：
// - **可回滚**：拷贝/入库任何一步失败，半成品目录必须清掉，库里不留孤儿目录
// - **批量容错**：Web 版导入逐条 try，一个条目坏了不拖垮整批，错误逐条带回前端
// - **源/目标重叠校验**：误选库目录本身或其祖先目录会递归自拷直到磁盘爆满
// - **item_id 只用 ASCII**：它会拼进内容服务器 URL 路径段；中文等非 ASCII
//   名称清洗后会落空，用名称的稳定短哈希兜底，而不是所有中文壁纸都叫 "custom"
// - **幂等**：同一文件重复导入（同名同大小）直接复用已有条目，不产生
//   `name-2`/`name-3` 垃圾副本
// - **不阻塞 UI**：两个导入命令都是 async + spawn_blocking，大目录拷贝
//   不会冻结主线程

/// 单次导入的体积上限：防误选巨型目录（如整个下载目录）把磁盘拷爆
const MAX_IMPORT_BYTES: u64 = 20 << 30; // 20 GiB

const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mov", "mkv", "m4v", "avi"];
const GIF_EXTS: &[&str] = &["gif"];
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "avif"];
const WEB_EXTS: &[&str] = &["html", "htm"];
/// 文件本身可直接当预览图的扩展名（内容服务器按 preview.<ext> 提供）
const PREVIEW_COPY_EXTS: &[&str] = &["gif", "png", "jpg", "jpeg", "webp"];

fn ext_lower(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// 扩展名 → 壁纸类型；None = 不支持导入
fn classify_ext(ext: &str) -> Option<&'static str> {
    if VIDEO_EXTS.contains(&ext) {
        Some("video")
    } else if GIF_EXTS.contains(&ext) {
        Some("gif")
    } else if IMAGE_EXTS.contains(&ext) {
        Some("image")
    } else if WEB_EXTS.contains(&ext) {
        Some("web")
    } else {
        None
    }
}

fn supported_exts_hint() -> &'static str {
    "支持：mp4/webm/mov/mkv/avi（视频）、gif、png/jpg/webp/bmp/avif（图片）、html（网页）、或包含 project.json 的壁纸目录"
}

fn human_bytes(n: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

/// FNV-1a 32bit：非 ASCII 名称的稳定兜底哈希（不引入额外依赖）
fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// 清洗出可作目录名的 item_id 词干：保留 ASCII 字母数字与 `-`/`_`，其余
/// 字符折叠成单个 `-`，去首尾 `-`，最长 64。纯非 ASCII 名称（如中文）
/// 清洗后为空时，用名称哈希兜底——稳定、不撞名、仍有辨识度。
fn sanitize_id_stem(name: &str) -> String {
    let mut out = String::with_capacity(name.len().min(64));
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    let capped: String = trimmed.chars().take(64).collect();
    let capped = capped.trim_end_matches('-');
    if capped.is_empty() {
        format!("custom-{:08x}", fnv1a32(name.as_bytes()))
    } else {
        capped.to_string()
    }
}

/// 源与库根不得重叠（canonicalize 后双向检查）。库根在 app_data 内，
/// 误选库目录本身或其祖先（如整个 app_data）会递归自拷直到磁盘爆满。
fn ensure_no_overlap(dest_root: &Path, src: &Path) -> Result<(), String> {
    let canon_root = std::fs::canonicalize(dest_root).map_err(|e| e.to_string())?;
    let canon_src = std::fs::canonicalize(src).map_err(|e| e.to_string())?;
    if canon_src.starts_with(&canon_root) {
        return Err("所选内容已在壁纸库内，无需导入".into());
    }
    if canon_root.starts_with(&canon_src) {
        return Err("不能导入包含壁纸库自身的目录".into());
    }
    Ok(())
}

/// 目标盘剩余空间预检：拷贝中途 ENOSPC 的报错不直观，提前拦下
fn ensure_disk_space(dest_root: &Path, needed: u64) -> Result<(), String> {
    let avail =
        fs2::available_space(dest_root).map_err(|e| format!("无法读取磁盘可用空间: {e}"))?;
    if needed > avail {
        return Err(format!(
            "磁盘空间不足：需要 {}，可用 {}",
            human_bytes(needed),
            human_bytes(avail)
        ));
    }
    Ok(())
}

#[derive(Default)]
struct CopyStats {
    bytes: u64,
    files: u64,
}

/// 递归拷贝：跳过符号链接与特殊文件（防循环链接/拷入链接目标），累计体积
/// 超 `MAX_IMPORT_BYTES` 立即中止。**中止/失败时目标目录由调用方负责回滚。**
fn copy_dir_filtered(from: &Path, to: &Path, stats: &mut CopyStats) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        // file_type 不跟随符号链接，能识别出 symlink 本身
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        if ft.is_symlink() {
            tracing::warn!("import: 跳过符号链接 {}", entry.path().display());
            continue;
        }
        let target = to.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_filtered(&entry.path(), &target, stats)?;
        } else if ft.is_file() {
            let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
            stats.bytes += len;
            if stats.bytes > MAX_IMPORT_BYTES {
                return Err(format!(
                    "导入内容超过大小上限 {}",
                    human_bytes(MAX_IMPORT_BYTES)
                ));
            }
            std::fs::copy(entry.path(), &target)
                .map_err(|e| format!("拷贝 {} 失败: {e}", entry.file_name().to_string_lossy()))?;
            stats.files += 1;
        }
        // fifo/socket/设备文件等：跳过
    }
    Ok(())
}

/// 与 copy_dir_filtered 同口径的体积预估（磁盘预检用）：跳过符号链接与特殊文件
fn measure_dir(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            match e.file_type() {
                Ok(ft) if ft.is_dir() => total += measure_dir(&e.path()),
                Ok(ft) if ft.is_file() => {
                    total += e.metadata().map(|m| m.len()).unwrap_or(0);
                }
                _ => {}
            }
        }
    }
    total
}

/// 在 dest_root 下找一个不冲突的目录名：clean 被占则 clean-2、clean-3…
fn unique_dest_dir(dest_root: &Path, clean: &str) -> PathBuf {
    let mut dest = dest_root.join(clean);
    let mut n = 2u32;
    while dest.exists() {
        dest = dest_root.join(format!("{clean}-{n}"));
        n += 1;
    }
    dest
}

/// 单文件去重：库里已有「同名且同大小」的文件 → 返回所在条目 id。
/// 只看名称+大小、不哈希全文：库可能很大，导入是低频操作，权衡是
/// 「同名同大小但内容不同」的极端情况会误判为重复，可接受，在此注明。
fn find_duplicate_file_item(dest_root: &Path, file_name: &str, len: u64) -> Option<String> {
    for entry in std::fs::read_dir(dest_root).ok()?.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let candidate = entry.path().join(file_name);
        if candidate.is_file() && candidate.metadata().map(|m| m.len()).unwrap_or(0) == len {
            return Some(entry.file_name().to_string_lossy().to_string());
        }
    }
    None
}

/// 导入核心结果（纯数据，不依赖 AppHandle，便于单测）
#[derive(Debug)]
struct ImportCore {
    item_id: String,
    title: String,
    wtype: String,
    size_bytes: i64,
    file_count: i64,
    /// 命中重复：未拷贝新文件，复用已有目录（失败回滚时**不得**删它）
    duplicate: bool,
}

/// 导入核心：把 src 拷贝进 dest_root 并解析元数据。
/// 任何失败都保证 dest_root 下不留半成品目录。
fn import_into_library(dest_root: &Path, src: &Path) -> Result<ImportCore, String> {
    if !src.exists() {
        return Err(format!("路径不存在: {}", src.display()));
    }
    ensure_no_overlap(dest_root, src)?;

    let raw_name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "custom".into());
    let stem = if src.is_dir() {
        raw_name.clone()
    } else {
        src.file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "custom".into())
    };
    let clean = sanitize_id_stem(&stem);

    if src.is_dir() {
        import_dir_into(dest_root, src, &clean, &stem)
    } else if src.is_file() {
        import_file_into(dest_root, src, &clean, &stem)
    } else {
        Err("只支持导入普通文件或目录".into())
    }
}

fn import_file_into(
    dest_root: &Path,
    src: &Path,
    clean: &str,
    stem: &str,
) -> Result<ImportCore, String> {
    let ext = ext_lower(src);
    let Some(wtype) = classify_ext(&ext) else {
        return Err(format!(
            "不支持的文件类型 .{ext}。{}",
            supported_exts_hint()
        ));
    };
    let len = src.metadata().map_err(|e| e.to_string())?.len();
    if len == 0 {
        return Err(format!("文件为空: {}", src.display()));
    }
    if len > MAX_IMPORT_BYTES {
        return Err(format!(
            "文件超过大小上限 {}",
            human_bytes(MAX_IMPORT_BYTES)
        ));
    }

    let file_name = src
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();

    // 幂等：同名同大小已在库 → 直接复用，不产生副本
    if let Some(existing_id) = find_duplicate_file_item(dest_root, &file_name, len) {
        let dir = dest_root.join(&existing_id);
        let (ptype, ptitle) = parse_project(&dir);
        return Ok(ImportCore {
            item_id: existing_id,
            title: if ptitle != "未命名" {
                ptitle
            } else {
                stem.to_string()
            },
            wtype: if ptype != "unknown" {
                ptype
            } else {
                wtype.to_string()
            },
            size_bytes: dir_size(&dir),
            file_count: dir_count(&dir),
            duplicate: true,
        });
    }

    ensure_disk_space(dest_root, len)?;
    let dest = unique_dest_dir(dest_root, clean);
    let item_id = dest
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| clean.to_string());

    // 拷贝；失败即回滚，不留半成品
    let copy_result = (|| -> Result<(), String> {
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        // 网页壁纸统一以 index.html 落地（引擎按此约定加载）
        let target_name = if WEB_EXTS.contains(&ext.as_str()) {
            "index.html".to_string()
        } else {
            file_name.clone()
        };
        std::fs::copy(src, dest.join(&target_name)).map_err(|e| format!("拷贝失败: {e}"))?;
        // 图片/gif 自身即可作预览；视频抽首帧做封面（复用系统壁纸同步的
        // 抽帧实现；抽帧失败只记日志不阻塞导入 —— 封面是锦上添花）。
        // 注意：按真实扩展名只拷一份，旧实现把同一源同时写成 preview.gif +
        // preview.png 两个文件，扩展名与内容对不上，纯粹是 bug。
        if PREVIEW_COPY_EXTS.contains(&ext.as_str()) {
            let pext = if ext == "jpeg" { "jpg" } else { ext.as_str() };
            std::fs::copy(src, dest.join(format!("preview.{pext}")))
                .map_err(|e| format!("生成预览失败: {e}"))?;
        } else if VIDEO_EXTS.contains(&ext.as_str()) {
            let video = dest.join(&target_name);
            let out = dest.join("preview.png");
            if let Err(e) = crate::system_wallpaper::video_poster_png(&video, &out) {
                tracing::warn!("导入视频抽帧失败（不影响导入）: {e}");
            }
        }
        Ok(())
    })();
    if let Err(e) = copy_result {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }

    Ok(ImportCore {
        item_id,
        // 单文件导入没有 project.json，用原始文件名做标题（比「未命名」有用）
        title: stem.to_string(),
        wtype: wtype.to_string(),
        size_bytes: dir_size(&dest),
        file_count: dir_count(&dest),
        duplicate: false,
    })
}

fn import_dir_into(
    dest_root: &Path,
    src: &Path,
    clean: &str,
    dir_name: &str,
) -> Result<ImportCore, String> {
    let needed = measure_dir(src);
    if needed == 0 {
        return Err(format!(
            "目录为空（或仅包含符号链接/特殊文件）: {}",
            src.display()
        ));
    }
    if needed > MAX_IMPORT_BYTES {
        return Err(format!(
            "目录超过大小上限 {}",
            human_bytes(MAX_IMPORT_BYTES)
        ));
    }
    ensure_disk_space(dest_root, needed)?;

    let dest = unique_dest_dir(dest_root, clean);
    let item_id = dest
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| clean.to_string());

    let mut stats = CopyStats::default();
    if let Err(e) = copy_dir_filtered(src, &dest, &mut stats) {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    if stats.files == 0 {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(format!(
            "目录为空（或仅包含符号链接/特殊文件）: {}",
            src.display()
        ));
    }

    // project.json 优先；类型缺失再按内容推断；标题缺失回退目录名
    let (ptype, ptitle) = parse_project(&dest);
    let wtype = if ptype != "unknown" {
        ptype
    } else {
        infer_type(&dest)
    };
    let title = if ptitle != "未命名" {
        ptitle
    } else {
        dir_name.to_string()
    };
    // 视频目录没有自带 preview.* 时抽首帧做封面（WE 工程自带 preview.gif 的不动）
    if wtype == "video" && !has_preview_file(&dest) {
        if let Some(video) = first_video_file(&dest) {
            let out = dest.join("preview.png");
            if let Err(e) = crate::system_wallpaper::video_poster_png(&video, &out) {
                tracing::warn!("目录导入抽帧失败（不影响导入）: {e}");
            }
        }
    }
    Ok(ImportCore {
        item_id,
        title,
        wtype,
        size_bytes: stats.bytes as i64,
        file_count: stats.files as i64,
        duplicate: false,
    })
}

/// library_items  upsert：导入（自定义/Web）与重复命中共用一条写路径
fn upsert_library_item(conn: &Connection, core: &ImportCore) -> Result<(), String> {
    conn.execute(
        "INSERT INTO library_items(item_id, title, type, size_bytes, file_count, downloaded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())
         ON CONFLICT(item_id) DO UPDATE SET title = ?2, type = ?3, size_bytes = ?4, file_count = ?5",
        rusqlite::params![core.item_id, core.title, core.wtype, core.size_bytes, core.file_count],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 一键导入 Web 版数据（壁纸文件 + 工坊元数据）。
/// 逐条容错：单个条目失败记入 failed 并继续，不拖垮整批。
#[tauri::command]
pub async fn library_import_from_web(
    app: AppHandle,
    web_data_dir: String,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || import_from_web_impl(&app, &web_data_dir))
        .await
        .map_err(|e| e.to_string())?
}

fn import_from_web_impl(app: &AppHandle, web_data_dir: &str) -> Result<serde_json::Value, String> {
    let src = Path::new(web_data_dir);
    if !src.is_dir() {
        return Err(format!("目录不存在: {web_data_dir}"));
    }
    let wallpapers_src = src.join("wallpapers");
    if !wallpapers_src.is_dir() {
        return Err("未找到 wallpapers/ 子目录（请选择 Web 版 apps/server/data 目录）".into());
    }
    let dest_root = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("wallpapers");
    std::fs::create_dir_all(&dest_root).map_err(|e| e.to_string())?;
    // 源就是库自身（或库的祖先）时逐条拷贝会变成自我复制，整体拦下
    ensure_no_overlap(&dest_root, &wallpapers_src)?;
    ensure_disk_space(&dest_root, measure_dir(&wallpapers_src))?;

    let db = app.state::<Arc<Mutex<Connection>>>();
    let mut imported = 0usize;
    let mut skipped = 0usize;
    let mut failed: Vec<serde_json::Value> = Vec::new();

    for entry in std::fs::read_dir(&wallpapers_src).map_err(|e| e.to_string())? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                failed.push(json!({ "id": "?", "error": e.to_string() }));
                continue;
            }
        };
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let item_id = entry.file_name().to_string_lossy().to_string();
        // 隐藏目录（.DS_Store / .Trash 之类）不是壁纸
        if item_id.starts_with('.') {
            continue;
        }
        let dest = dest_root.join(&item_id);

        // 拷贝：已是目录 → 跳过拷贝但仍刷新元数据；存在同名残留文件 → 清掉再拷
        let mut copied = false;
        if dest.is_dir() {
            skipped += 1;
        } else {
            if dest.exists() {
                if let Err(e) = std::fs::remove_file(&dest) {
                    failed
                        .push(json!({ "id": item_id, "error": format!("清理残留文件失败: {e}") }));
                    continue;
                }
            }
            let mut stats = CopyStats::default();
            match copy_dir_filtered(&entry.path(), &dest, &mut stats) {
                Ok(()) if stats.files > 0 => copied = true,
                Ok(()) => {
                    let _ = std::fs::remove_dir_all(&dest);
                    failed.push(json!({ "id": item_id, "error": "目录为空" }));
                    continue;
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dest);
                    failed.push(json!({ "id": item_id, "error": e }));
                    continue;
                }
            }
        }

        // 元数据 upsert：新拷的与跳过的都刷新（修正类型/体积/文件数）
        let (ptype, ptitle) = parse_project(&dest);
        let core = ImportCore {
            item_id: item_id.clone(),
            title: ptitle,
            wtype: if ptype != "unknown" {
                ptype
            } else {
                infer_type(&dest)
            },
            size_bytes: dir_size(&dest),
            file_count: dir_count(&dest),
            duplicate: false,
        };
        let r = {
            let conn = db.lock().map_err(|e| e.to_string())?;
            upsert_library_item(&conn, &core)
        };
        if let Err(e) = r {
            failed.push(json!({ "id": item_id, "error": format!("写入数据库失败: {e}") }));
            continue;
        }
        if copied {
            imported += 1;
        }
    }
    Ok(json!({ "imported": imported, "skipped": skipped, "failed": failed }))
}

/// 壁纸文件选择器的扩展名过滤（与 classify_ext 的支持列表一一对应）
const PICK_FILTER_EXTS: &[&str] = &[
    "mp4", "webm", "mov", "mkv", "m4v", "avi", "gif", "png", "jpg", "jpeg", "webp", "bmp", "avif",
    "html", "htm",
];

/// 导入自定义壁纸：原生文件选择框（**支持多选**）→ 批量导入。
#[tauri::command]
pub async fn library_import_custom_pick(app: AppHandle) -> Result<serde_json::Value, String> {
    // blocking 对话框不能阻塞主线程/async 执行器 → 用 spawn_blocking 放到阻塞线程池
    let app_pick = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app_pick
            .dialog()
            .file()
            .add_filter("壁纸", PICK_FILTER_EXTS)
            .blocking_pick_files()
            .map(|fs| {
                fs.iter()
                    .filter_map(|f| f.as_path().map(|p| p.to_path_buf()))
                    .collect::<Vec<_>>()
            })
    })
    .await
    .map_err(|e| e.to_string())?;

    let Some(paths) = picked else {
        return Ok(json!({ "cancelled": true }));
    };
    if paths.is_empty() {
        return Ok(json!({ "cancelled": true }));
    }
    import_custom_batch_run(app, paths).await
}

/// 导入自定义壁纸：原生文件夹选择框 → 作为完整壁纸目录导入
/// （WE 工程目录，含 project.json 时优先）。
#[tauri::command]
pub async fn library_import_folder_pick(app: AppHandle) -> Result<serde_json::Value, String> {
    let app_pick = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app_pick
            .dialog()
            .file()
            .blocking_pick_folder()
            .and_then(|f| f.as_path().map(|p| p.to_path_buf()))
    })
    .await
    .map_err(|e| e.to_string())?;

    let Some(path) = picked else {
        return Ok(json!({ "cancelled": true }));
    };
    import_custom_batch_run(app, vec![path]).await
}

/// 导入自定义壁纸：把单个壁纸文件/目录拷贝到本地库。
///
/// 支持：mp4/webm/mov/mkv/avi → video；gif → gif；png/jpg/jpeg/webp/bmp/avif → image；
/// html/htm → web；目录 → 作为完整壁纸目录导入（含 project.json 时优先）。
/// 重复导入同一文件（同名同大小）幂等返回已有条目，不产生副本。
#[tauri::command]
pub async fn library_import_custom(
    app: AppHandle,
    source_path: String,
) -> Result<serde_json::Value, String> {
    let app2 = app.clone();
    let r =
        tauri::async_runtime::spawn_blocking(move || import_one(&app2, Path::new(&source_path)))
            .await
            .map_err(|e| e.to_string())??;
    Ok(json!({
        "imported": 1,
        "item_id": r.core.item_id,
        "title": r.core.title,
        "type": r.core.wtype,
        "duplicate": r.core.duplicate,
    }))
}

/// 批量导入（拖拽/多选用）：逐条容错，单条失败不拖垮整批。
#[tauri::command]
pub async fn library_import_custom_batch(
    app: AppHandle,
    paths: Vec<String>,
) -> Result<serde_json::Value, String> {
    let paths: Vec<std::path::PathBuf> = paths.iter().map(std::path::PathBuf::from).collect();
    if paths.is_empty() {
        return Err("没有可导入的路径".into());
    }
    import_custom_batch_run(app, paths).await
}

async fn import_custom_batch_run(
    app: AppHandle,
    paths: Vec<std::path::PathBuf>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || import_custom_batch_impl(&app, &paths))
        .await
        .map_err(|e| e.to_string())?
}

/// 批量导入实现：每条路径独立走「拷贝 → 入库（失败回滚）」，互不影响。
fn import_custom_batch_impl(
    app: &AppHandle,
    paths: &[std::path::PathBuf],
) -> Result<serde_json::Value, String> {
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut failed: Vec<serde_json::Value> = Vec::new();
    let mut imported = 0usize;
    let mut duplicates = 0usize;

    for path in paths {
        match import_one(app, path) {
            Ok(r) => {
                if r.core.duplicate {
                    duplicates += 1;
                } else {
                    imported += 1;
                }
                items.push(json!({
                    "path": path.display().to_string(),
                    "item_id": r.core.item_id,
                    "title": r.core.title,
                    "type": r.core.wtype,
                    "duplicate": r.core.duplicate,
                }));
            }
            Err(e) => {
                failed.push(json!({ "path": path.display().to_string(), "error": e }));
            }
        }
    }
    Ok(json!({
        "imported": imported,
        "duplicates": duplicates,
        "items": items,
        "failed": failed,
    }))
}

struct ImportOne {
    core: ImportCore,
}

/// 单条导入：拷贝（可回滚）→ 入库（失败回滚新拷目录）。
/// 两条命令（单路径/批量）共用的唯一写路径。
fn import_one(app: &AppHandle, src: &Path) -> Result<ImportOne, String> {
    let dest_root = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("wallpapers");
    std::fs::create_dir_all(&dest_root).map_err(|e| e.to_string())?;

    let core = import_into_library(&dest_root, src)?;

    // 入库失败回滚新拷的目录；重复命中的是既有目录，绝不能删
    let db = app.state::<Arc<Mutex<Connection>>>();
    let r = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        upsert_library_item(&conn, &core)
    };
    if let Err(e) = r {
        if !core.duplicate {
            let _ = std::fs::remove_dir_all(dest_root.join(&core.item_id));
        }
        return Err(e);
    }
    Ok(ImportOne { core })
}

/// 目录内是否已有预览图（扩展名清单与 local_preview_url 保持一致）
fn has_preview_file(dir: &Path) -> bool {
    ["gif", "png", "jpg", "webp"]
        .iter()
        .any(|e| dir.join(format!("preview.{e}")).is_file())
}

/// 目录内第一个视频文件（按文件名排序，与 read_dir 枚举顺序无关，结果可复现）
fn first_video_file(dir: &Path) -> Option<PathBuf> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| VIDEO_EXTS.contains(&ext_lower(Path::new(n)).as_str()))
        .collect();
    names.sort();
    names.into_iter().next().map(|n| dir.join(n))
}

/// 目录内推断壁纸类型（无 project.json 时）。
/// 按类型优先级取最优（video > gif > image > web），**与目录枚举顺序无关**——
/// 旧实现逐条目首个命中即返回，read_dir 顺序不定，混合目录结果不可复现。
/// preview.* 是预览图，不参与推断（否则带 preview.gif 的视频目录会被判成 gif）。
fn infer_type(dir: &Path) -> String {
    if dir.join("index.html").is_file() || dir.join("web/index.html").is_file() {
        return "web".into();
    }
    if dir.join("scenes/scene.pkg").is_file() || dir.join("scene.pkg").is_file() {
        return "scene".into();
    }
    let rank = |t: &str| match t {
        "video" => 0,
        "gif" => 1,
        "image" => 2,
        "web" => 3,
        _ => 9,
    };
    let mut best: Option<&'static str> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            if name.starts_with("preview.") {
                continue;
            }
            if let Some(t) = classify_ext(&ext_lower(&e.path())) {
                if best.map_or(true, |b| rank(t) < rank(b)) {
                    best = Some(t);
                }
            }
        }
    }
    best.unwrap_or("image").to_string()
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

// ---------- WE 网页壁纸用户属性 ----------

/// 壁纸库根目录 `<app_data>/wallpapers`
pub(crate) fn wallpapers_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|d| d.join("wallpapers"))
        .map_err(|e| e.to_string())
}

/// 单个壁纸的目录 `<app_data>/wallpapers/<item_id>`
pub(crate) fn item_dir(app: &AppHandle, item_id: &str) -> Result<std::path::PathBuf, String> {
    Ok(wallpapers_dir(app)?.join(item_id))
}

/// 壁纸文件是否真实存在。
///
/// 判据是「目录存在**且非空**」而不只是 `is_dir()`：删除内容但留下空壳目录
/// （某些同步工具、或 remove_dir_all 中途失败）同样意味着壁纸不可用。
pub(crate) fn item_files_exist(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

/// web 壁纸的属性定义列表（project.json 默认值 + 用户覆盖合并后的当前值）。
/// 非 web 类型/无属性时返回空数组。
#[tauri::command(rename = "library_item_props")]
pub fn item_props(app: AppHandle, item_id: String) -> Result<Vec<we_props::WebPropDef>, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let dir = wallpapers_dir(&app)?;
    Ok(we_props::describe(&conn, &dir, &item_id))
}

/// 按 item_id 查标题（托盘「壁纸设置」需要给配置弹窗一个可读标题）。
/// 查不到（本地库无此条/工坊元数据缺失）时回退 item_id 本身，不报错 ——
/// 配置弹窗的标题只是辅助信息，不该因为标题缺失而打不开。
#[tauri::command(rename = "library_item_title")]
pub fn item_title(app: AppHandle, item_id: String) -> Result<String, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let title: Option<String> = conn
        .query_row(
            "SELECT COALESCE(NULLIF(w.title,''), NULLIF(l.title,''), ?1)
             FROM library_items l LEFT JOIN workshop_items w ON w.id = l.item_id
             WHERE l.item_id = ?1",
            rusqlite::params![item_id, item_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    Ok(title.unwrap_or(item_id))
}

/// 保存用户属性覆盖（wire 格式值），并对正在应用该壁纸的窗口热更新（免刷新生效）
#[tauri::command(rename = "library_set_item_props")]
pub fn set_item_props(
    app: AppHandle,
    item_id: String,
    values: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        we_props::set_overrides(&conn, &item_id, &values)?;
    }
    apply_live(&app, &item_id)
}

/// 清除用户属性覆盖（回到 project.json 默认值），并对已应用窗口热更新
#[tauri::command(rename = "library_reset_item_props")]
pub fn reset_item_props(app: AppHandle, item_id: String) -> Result<(), String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        we_props::set_overrides(&conn, &item_id, &serde_json::Map::new())?;
    }
    apply_live(&app, &item_id)
}

/// file/directory 类型属性：弹出系统选择框后写覆盖值并对已应用窗口热更新。
/// - file：拷入壁纸目录（we-props/ 子目录），存相对壁纸根的路径（下发时按入口目录补前缀）；
///   选择器过滤器按 project.json `fileType`（image/video/audio，缺省 image）
/// - directory：不拷贝，存所选目录的绝对路径（与 WE 的目录属性存储语义一致）
/// 返回 {cancelled: true} 或 {value: "<路径>"}。
#[tauri::command(rename = "library_set_item_prop_file")]
pub async fn set_item_prop_file(
    app: AppHandle,
    item_id: String,
    prop_name: String,
) -> Result<serde_json::Value, String> {
    use tauri_plugin_dialog::DialogExt;

    // 校验属性存在且类型受支持，同时取 fileType 决定选择器过滤器
    let db = app.state::<Arc<Mutex<Connection>>>();
    let dir = wallpapers_dir(&app)?;
    let (is_dir, file_type) = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let defs = we_props::describe(&conn, &dir, &item_id);
        match defs.iter().find(|d| d.name == prop_name) {
            Some(d) if d.ptype == "file" => (false, d.file_type.clone()),
            Some(d) if d.ptype == "directory" => (true, None),
            Some(d) => return Err(format!("属性 {prop_name} 是 {} 类型，非文件/目录", d.ptype)),
            None => return Err(format!("属性 {prop_name} 不存在")),
        }
    };

    let app_pick = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let dialog = app_pick.dialog().file();
        let picked = if is_dir {
            dialog.blocking_pick_folder()
        } else {
            dialog
                .add_filter("文件", file_filter(file_type.as_deref()))
                .blocking_pick_file()
        };
        picked.and_then(|f| f.as_path().map(|p| p.to_path_buf()))
    })
    .await
    .map_err(|e| e.to_string())?;

    let Some(src) = picked else {
        return Ok(json!({ "cancelled": true }));
    };

    // directory：绝对路径直接作为覆盖值（页面经内容服务器取不到本地目录，
    // 该值主要供壁纸读取路径文本/上层能力使用，与 WE 存储语义一致）
    if is_dir {
        let abs = src.to_string_lossy().to_string();
        {
            let conn = db.lock().map_err(|e| e.to_string())?;
            we_props::set_single_override(&conn, &item_id, &prop_name, json!(abs))?;
        }
        apply_live(&app, &item_id)?;
        return Ok(json!({ "value": abs }));
    }

    // 拷入 we-props/{prop}_{原文件名}（属性名前缀防冲突）
    let file_name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "image.png".into());
    let dest_name = format!("{prop_name}_{file_name}");
    let dest_dir = dir.join(&item_id).join("we-props");
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    let dest = dest_dir.join(&dest_name);
    std::fs::copy(&src, &dest).map_err(|e| format!("拷贝文件失败: {e}"))?;
    let rel = format!("we-props/{dest_name}");

    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        we_props::set_single_override(&conn, &item_id, &prop_name, json!(rel))?;
    }
    apply_live(&app, &item_id)?;
    Ok(json!({ "value": rel }))
}

/// fileType → 文件选择器扩展名过滤器（缺省按图片；壁纸最常见的是贴图）
fn file_filter(file_type: Option<&str>) -> &'static [&'static str] {
    match file_type {
        Some("video") => &["mp4", "webm", "mov", "m4v"],
        Some("audio") => &["mp3", "ogg", "wav", "flac", "m4a"],
        _ => &["png", "jpg", "jpeg", "webp", "gif", "bmp"],
    }
}

/// 把该壁纸当前的完整属性表热更新到所有正在应用它的壁纸窗口
fn apply_live(app: &AppHandle, item_id: &str) -> Result<(), String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let dir = wallpapers_dir(app)?;
    let props = we_props::effective_props(&conn, &dir, item_id);
    let displays: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT display_id FROM wallpaper_sessions WHERE item_id = ?1 AND item_id != ''",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<String> = stmt
            .query_map([item_id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    drop(conn);
    if displays.is_empty() {
        return Ok(());
    }
    let js = format!(
        "window.__wp && window.__wp.updateWebProps({})",
        serde_json::to_string(&props).map_err(|e| e.to_string())?
    );
    for d in displays {
        if let Some(w) = app.get_webview_window(&format!("wallpaper-{d}")) {
            let _ = w.eval(&js);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wpem-lib-test-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE library_items(item_id TEXT PRIMARY KEY, title TEXT, type TEXT,
                 size_bytes INTEGER, file_count INTEGER, downloaded_at INTEGER);
             CREATE TABLE wallpaper_sessions(display_id TEXT PRIMARY KEY, item_id TEXT, config_json TEXT);
             CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE downloads(id INTEGER PRIMARY KEY, item_id TEXT, status TEXT);
             CREATE TABLE playlists(id INTEGER PRIMARY KEY, name TEXT, item_ids TEXT, interval_sec INTEGER);
             CREATE TABLE favorites(user_id INTEGER, item_id TEXT);
             CREATE TABLE workshop_items(id TEXT PRIMARY KEY, title TEXT);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn item_files_exist_requires_non_empty_dir() {
        let root = tmpdir("exists");
        // 目录不存在
        assert!(!item_files_exist(&root.join("nope")));
        // 空壳目录同样算丢失（删了内容但留下目录）
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(!item_files_exist(&empty));
        // 有内容才算存在
        let ok = root.join("ok");
        std::fs::create_dir_all(&ok).unwrap();
        std::fs::write(ok.join("project.json"), "{}").unwrap();
        assert!(item_files_exist(&ok));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 构造一个带筛选所需列的库表 + 工坊缓存
    fn filter_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE library_items(item_id TEXT PRIMARY KEY, title TEXT, type TEXT,
                 tags TEXT, size_bytes INTEGER, file_count INTEGER, downloaded_at INTEGER);
             CREATE TABLE workshop_items(id TEXT PRIMARY KEY, preview_url TEXT, tags TEXT, type TEXT);
             INSERT INTO library_items VALUES
               ('a','Anime Girl','scene','[\"Scene\",\"Anime\",\"Everyone\"]',1000,3,100),
               ('b','Landscape 4K','video','[\"Video\",\"Landscape\",\"Mature\"]',5000,1,200),
               ('c','100% Custom','web','[]',300,2,300);
             INSERT INTO workshop_items VALUES
               ('c','','[\"Web\",\"Abstract\"]','web');",
        )
        .unwrap();
        conn
    }

    /// 复刻 library_list 的 WHERE 构造，验证参数绑定与标签语义。
    /// （命令本身依赖 AppHandle，无法在单测里跑，这里验证 SQL 层。）
    fn run_filter(conn: &Connection, where_sql: &str, binds: &[&str]) -> Vec<String> {
        let sql = format!(
            "SELECT l.item_id FROM library_items l
             LEFT JOIN workshop_items w ON w.id = l.item_id
             WHERE {where_sql} ORDER BY l.item_id"
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }

    const TAG_EXISTS: &str = "EXISTS (SELECT 1 FROM json_each(
            CASE WHEN json_valid(l.tags) THEN l.tags ELSE '[]' END
         ) WHERE value = ?
       ) OR EXISTS (SELECT 1 FROM json_each(
            CASE WHEN json_valid(w.tags) THEN w.tags ELSE '[]' END
         ) WHERE value = ?
       )";

    #[test]
    fn tag_filter_matches_local_or_workshop_tags() {
        let conn = filter_db();
        // 本地 tags 命中
        assert_eq!(
            run_filter(&conn, &format!("({TAG_EXISTS})"), &["Anime", "Anime"]),
            vec!["a"]
        );
        // 本地 tags 为空、工坊缓存命中（自定义导入壁纸的兜底路径）
        assert_eq!(
            run_filter(&conn, &format!("({TAG_EXISTS})"), &["Abstract", "Abstract"]),
            vec!["c"]
        );
    }

    #[test]
    fn multiple_tags_are_and_not_or() {
        let conn = filter_db();
        let two = format!("({TAG_EXISTS}) AND ({TAG_EXISTS})");
        // Scene AND Anime 都在 a 上 → 命中
        assert_eq!(
            run_filter(&conn, &two, &["Scene", "Scene", "Anime", "Anime"]),
            vec!["a"]
        );
        // Anime AND Landscape 分属不同条目 → 与 Steam 一致，取交集为空
        assert!(run_filter(&conn, &two, &["Anime", "Anime", "Landscape", "Landscape"]).is_empty());
    }

    #[test]
    fn excluded_tag_removes_matching_items() {
        let conn = filter_db();
        let out = run_filter(&conn, &format!("NOT ({TAG_EXISTS})"), &["Mature", "Mature"]);
        assert_eq!(out, vec!["a", "c"], "带 Mature 的 b 应被排除");
    }

    #[test]
    fn like_wildcards_in_query_are_escaped() {
        let conn = filter_db();
        // 「100%」里的 % 若不转义会退化成「匹配一切」
        let esc = "100%"
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{esc}%");
        let mut stmt = conn
            .prepare(
                "SELECT item_id FROM library_items WHERE title LIKE ? ESCAPE '\\' ORDER BY item_id",
            )
            .unwrap();
        let out: Vec<String> = stmt
            .query_map([&pattern], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(out, vec!["c"], "应只匹配标题真含 100% 的条目");
    }

    #[test]
    fn order_by_clause_is_whitelisted() {
        // 任意输入都只能落到白名单里的固定字符串，杜绝 ORDER BY 注入
        assert_eq!(order_by_clause(None), "ORDER BY l.downloaded_at DESC");
        assert_eq!(
            order_by_clause(Some("size_asc")),
            "ORDER BY l.size_bytes ASC"
        );
        assert_eq!(
            order_by_clause(Some("l.title; DROP TABLE library_items")),
            "ORDER BY l.downloaded_at DESC"
        );
    }

    #[test]
    fn malformed_tags_json_does_not_break_query() {
        let conn = filter_db();
        conn.execute(
            "INSERT INTO library_items VALUES ('d','Bad','scene','not json',1,1,400)",
            [],
        )
        .unwrap();
        // json_valid 兜底：坏数据不该让整条 SQL 报错
        let out = run_filter(&conn, &format!("({TAG_EXISTS})"), &["Anime", "Anime"]);
        assert_eq!(out, vec!["a"]);
    }

    #[test]
    fn purge_clears_all_traces_but_keeps_favorites_and_workshop_cache() {
        let conn = mem_db();
        conn.execute_batch(
            "INSERT INTO library_items(item_id,title,type) VALUES ('a','A','scene'),('b','B','web');
             INSERT INTO wallpaper_sessions VALUES ('scr1','a','{}'),('scr2','b','{}');
             INSERT INTO settings VALUES ('web_props:a','{\"x\":1}'),('web_props:b','{\"y\":2}');
             INSERT INTO downloads(id,item_id,status) VALUES (1,'a','done'),(2,'a','downloading');
             INSERT INTO favorites VALUES (1,'a');
             INSERT INTO workshop_items VALUES ('a','A');",
        )
        .unwrap();

        purge_item_records(&conn, "a").unwrap();

        let cnt = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            cnt("SELECT COUNT(*) FROM library_items WHERE item_id='a'"),
            0
        );
        assert_eq!(
            cnt("SELECT COUNT(*) FROM wallpaper_sessions WHERE item_id='a'"),
            0
        );
        assert_eq!(
            cnt("SELECT COUNT(*) FROM settings WHERE key='web_props:a'"),
            0,
            "属性覆盖必须清掉，否则重下同一壁纸旧配置会复活"
        );
        assert_eq!(
            cnt("SELECT COUNT(*) FROM downloads WHERE item_id='a' AND status='done'"),
            0
        );
        assert_eq!(
            cnt("SELECT COUNT(*) FROM downloads WHERE item_id='a' AND status='downloading'"),
            1,
            "进行中的下载任务不该被清掉"
        );
        // 无关条目不受影响
        assert_eq!(
            cnt("SELECT COUNT(*) FROM library_items WHERE item_id='b'"),
            1
        );
        assert_eq!(
            cnt("SELECT COUNT(*) FROM settings WHERE key='web_props:b'"),
            1
        );
        // 收藏与工坊缓存刻意保留
        assert_eq!(cnt("SELECT COUNT(*) FROM favorites WHERE item_id='a'"), 1);
        assert_eq!(cnt("SELECT COUNT(*) FROM workshop_items WHERE id='a'"), 1);
    }

    #[test]
    fn purge_removes_item_from_playlists_and_active_snapshot() {
        let conn = mem_db();
        conn.execute_batch(
            r#"INSERT INTO playlists(id,name,item_ids,interval_sec) VALUES
                 (1,'p1','["a","b","c"]',600),
                 (2,'p2','["b"]',600);
               INSERT INTO settings VALUES
                 ('active_playlist','{"id":1,"name":"p1","itemIds":["a","b","c"],"intervalSec":600}');"#,
        )
        .unwrap();

        purge_item_records(&conn, "a").unwrap();

        let ids: String = conn
            .query_row("SELECT item_ids FROM playlists WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(ids, r#"["b","c"]"#);
        // 不含该条目的播放列表不动
        let p2: String = conn
            .query_row("SELECT item_ids FROM playlists WHERE id=2", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(p2, r#"["b"]"#);
        // 激活快照与 playlists 表是两份独立数据，必须同步
        let snap: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key='active_playlist'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&snap).unwrap();
        let arr: Vec<&str> = v["itemIds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(arr, vec!["b", "c"], "轮播快照未同步会导致每轮空转一次");
    }

    #[test]
    fn purge_tolerates_malformed_playlist_json() {
        let conn = mem_db();
        conn.execute_batch(
            "INSERT INTO playlists(id,name,item_ids,interval_sec) VALUES (1,'bad','not json',600);",
        )
        .unwrap();
        // 不应 panic，坏数据跳过即可
        purge_item_records(&conn, "a").unwrap();
        let ids: String = conn
            .query_row("SELECT item_ids FROM playlists WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(ids, "not json");
    }
    // ---------- 导入核心（import_into_library 及辅助函数） ----------

    /// 建一个隔离的「库根 + 外部源目录」对，返回 (root, dest_root, src_root)
    fn import_fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let root = tmpdir(&format!("import-{tag}"));
        let dest_root = root.join("library");
        let src_root = root.join("incoming");
        std::fs::create_dir_all(&dest_root).unwrap();
        std::fs::create_dir_all(&src_root).unwrap();
        (root, dest_root, src_root)
    }

    #[test]
    fn sanitize_id_stem_keeps_ascii_and_collapses_separators() {
        assert_eq!(sanitize_id_stem("My Wallpaper_01"), "My-Wallpaper_01");
        assert_eq!(sanitize_id_stem("--a..b--"), "a-b");
        assert_eq!(sanitize_id_stem("wallpaper.mp4"), "wallpaper-mp4");
        // 超长截断到 64
        let long = "a".repeat(200);
        assert_eq!(sanitize_id_stem(&long).len(), 64);
    }

    #[test]
    fn sanitize_id_stem_non_ascii_falls_back_to_stable_hash() {
        let a = sanitize_id_stem("星空壁纸");
        let b = sanitize_id_stem("星空壁纸");
        let c = sanitize_id_stem("另一个壁纸");
        assert_eq!(a, b, "同名必须稳定");
        assert_ne!(a, c, "不同名不该撞车");
        assert!(a.starts_with("custom-"), "兜底前缀可读: {a}");
        // 不能再退化成所有人共用的 "custom"
        assert_ne!(a, "custom");
    }

    #[test]
    fn overlap_check_rejects_self_and_ancestor() {
        let (root, dest_root, src_root) = import_fixture("overlap");
        // 源在库内
        let inside = dest_root.join("item-1");
        std::fs::create_dir_all(&inside).unwrap();
        assert!(ensure_no_overlap(&dest_root, &inside).is_err());
        // 源是库的祖先
        assert!(ensure_no_overlap(&dest_root, &root).is_err());
        assert!(ensure_no_overlap(&dest_root, &dest_root).is_err());
        // 平级目录 OK
        assert!(ensure_no_overlap(&dest_root, &src_root).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_single_video_uses_filename_as_title() {
        let (root, dest_root, src_root) = import_fixture("video");
        let src = src_root.join("海边日落.mp4");
        std::fs::write(&src, b"fake-video-bytes").unwrap();

        let core = import_into_library(&dest_root, &src).unwrap();
        assert_eq!(core.wtype, "video");
        assert_eq!(core.title, "海边日落", "标题用原始文件名，不该是「未命名」");
        assert!(!core.duplicate);
        // 中文名 → 哈希兜底目录名，且文件确实拷过去了
        let dir = dest_root.join(&core.item_id);
        assert!(dir.join("海边日落.mp4").is_file());
        assert_eq!(core.file_count, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_same_file_twice_is_idempotent() {
        let (root, dest_root, src_root) = import_fixture("dup");
        let src = src_root.join("loop.mp4");
        std::fs::write(&src, b"same-content").unwrap();

        let first = import_into_library(&dest_root, &src).unwrap();
        let second = import_into_library(&dest_root, &src).unwrap();
        assert!(!first.duplicate);
        assert!(second.duplicate, "第二次导入应命中去重");
        assert_eq!(first.item_id, second.item_id);
        // 库里只有一个目录，没有 loop-2 副本
        assert_eq!(
            std::fs::read_dir(&dest_root).unwrap().count(),
            1,
            "重复导入不得产生 name-2 副本"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_same_name_different_size_creates_suffixed_dir() {
        let (root, dest_root, src_root) = import_fixture("suffix");
        let a = src_root.join("wp.mp4");
        std::fs::write(&a, b"aaaa").unwrap();
        let first = import_into_library(&dest_root, &a).unwrap();
        // 同名但内容/大小不同（模拟另一个目录下的同名文件）
        std::fs::write(&a, b"aaaa-bbbb").unwrap();
        let second = import_into_library(&dest_root, &a).unwrap();
        assert!(!second.duplicate);
        assert_ne!(first.item_id, second.item_id);
        assert!(second.item_id.starts_with(&format!("{}-", first.item_id)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_rejects_unsupported_and_empty_files() {
        let (root, dest_root, src_root) = import_fixture("reject");
        let bin = src_root.join("data.bin");
        std::fs::write(&bin, b"xxxx").unwrap();
        let err = import_into_library(&dest_root, &bin).unwrap_err();
        assert!(err.contains("不支持的文件类型"), "{err}");

        let empty = src_root.join("empty.mp4");
        std::fs::write(&empty, b"").unwrap();
        assert!(import_into_library(&dest_root, &empty).is_err());
        // 失败不留任何目录
        assert_eq!(std::fs::read_dir(&dest_root).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_image_writes_single_matching_preview() {
        let (root, dest_root, src_root) = import_fixture("preview");
        let src = src_root.join("pic.png");
        std::fs::write(&src, b"png-bytes").unwrap();
        let core = import_into_library(&dest_root, &src).unwrap();
        let dir = dest_root.join(&core.item_id);
        assert!(dir.join("preview.png").is_file());
        // 旧 bug：同一源被同时写成 preview.gif + preview.png
        assert!(
            !dir.join("preview.gif").exists(),
            "不得再生成对不上的 preview.gif"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_dir_prefers_project_json_and_rolls_back_on_empty() {
        let (root, dest_root, src_root) = import_fixture("dir");
        // 带 project.json 的目录：类型/标题以 project 为准
        let proj = src_root.join("cool-scene");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("project.json"),
            r#"{"type":"scene","title":"Cool Scene"}"#,
        )
        .unwrap();
        std::fs::write(proj.join("scene.pkg"), b"pkg").unwrap();
        let core = import_into_library(&dest_root, &proj).unwrap();
        assert_eq!(core.wtype, "scene");
        assert_eq!(core.title, "Cool Scene");
        assert_eq!(core.item_id, "cool-scene");

        // 空目录：报错且不留半成品
        let empty = src_root.join("empty-dir");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(import_into_library(&dest_root, &empty).is_err());
        assert!(!dest_root.join("empty-dir").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_dir_skips_symlinks() {
        let (root, dest_root, src_root) = import_fixture("symlink");
        let outside = root.join("secret.txt");
        std::fs::write(&outside, b"secret").unwrap();
        let proj = src_root.join("wp-dir");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("wallpaper.mp4"), b"video").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, proj.join("linked.txt")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, proj.join("linked.txt")).unwrap();

        let core = import_into_library(&dest_root, &proj).unwrap();
        let dir = dest_root.join(&core.item_id);
        assert!(dir.join("wallpaper.mp4").is_file());
        assert!(!dir.join("linked.txt").exists(), "符号链接不得被拷贝进库");
        assert_eq!(core.file_count, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn infer_type_is_deterministic_and_ignores_preview_files() {
        let root = tmpdir("infer");
        // 混合目录：preview.gif + 视频 → video（旧实现按枚举顺序，可能判成 gif）
        let d1 = root.join("d1");
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::write(d1.join("preview.gif"), b"g").unwrap();
        std::fs::write(d1.join("main.mp4"), b"v").unwrap();
        assert_eq!(infer_type(&d1), "video");
        // 只有 preview.png + 一张图片 → image（preview 不参与推断）
        let d2 = root.join("d2");
        std::fs::create_dir_all(&d2).unwrap();
        std::fs::write(d2.join("preview.png"), b"p").unwrap();
        std::fs::write(d2.join("bg.webp"), b"i").unwrap();
        assert_eq!(infer_type(&d2), "image");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn first_video_file_is_deterministic() {
        let root = tmpdir("firstvideo");
        // 多个视频时按文件名排序取第一个，与 read_dir 枚举顺序无关
        for n in ["b.webm", "a.mp4", "c.mov"] {
            std::fs::write(root.join(n), b"v").unwrap();
        }
        // 非视频文件不入选
        std::fs::write(root.join("cover.png"), b"p").unwrap();
        let got = first_video_file(&root).unwrap();
        assert_eq!(got.file_name().unwrap().to_string_lossy(), "a.mp4");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn has_preview_file_matches_local_preview_url_exts() {
        let root = tmpdir("haspreview");
        assert!(!has_preview_file(&root));
        std::fs::write(root.join("preview.png"), b"p").unwrap();
        assert!(has_preview_file(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn batch_import_items_are_independent() {
        // 批量导入的核心语义：单条失败不影响其它条目（import_custom_batch_impl
        // 是 import_one 的薄循环，这里在纯核心层验证同等语义）
        let (root, dest_root, src_root) = import_fixture("batch");
        let good1 = src_root.join("a.mp4");
        let good2 = src_root.join("b.png");
        let bad = src_root.join("c.xyz");
        std::fs::write(&good1, b"v").unwrap();
        std::fs::write(&good2, b"i").unwrap();
        std::fs::write(&bad, b"x").unwrap();

        let results: Vec<_> = [&good1, &bad, &good2]
            .iter()
            .map(|p| import_into_library(&dest_root, p))
            .collect();
        assert!(results[0].is_ok());
        assert!(results[1].is_err(), "不支持的扩展名必须失败");
        assert!(results[2].is_ok(), "坏条目不得拖垮后续条目");
        assert_eq!(results[0].as_ref().unwrap().wtype, "video");
        assert_eq!(results[2].as_ref().unwrap().wtype, "image");
        // 同批内重复（同一次拖入同一个文件两次）第二条命中去重
        let dup = import_into_library(&dest_root, &good1).unwrap();
        assert!(dup.duplicate);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_rejects_library_into_itself() {
        let (root, dest_root, _src_root) = import_fixture("selfcopy");
        // 试图把库目录自身导进来 → 必须报错而非递归自拷
        assert!(import_into_library(&dest_root, &dest_root).is_err());
        // 试图导入库的祖先目录 → 同样拦下
        assert!(import_into_library(&dest_root, &root).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
