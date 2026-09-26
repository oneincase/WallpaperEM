//! 壁纸分享（/share/*）：把本地库里的某张壁纸以不可枚举的链接开放给浏览器渲染。
//!
//! 权限模型：**shareId 即凭据**（128bit 随机 hex，不可枚举），只读、只到这一张
//! 壁纸的文件；控制面（/mcp、/api）与分享路由物理隔离。内容 token 永不出现在
//! 分享链路里 —— media 路由按 share→item 映射限死目录。
//!
//! 路由族：
//! - `GET /share/{id}`                落地页（预览图 + 打开按钮）
//! - `GET /share/{id}/render|/embed`  302 → 渲染页（query 按 resolve_item_config 复刻，
//!                                     `mediaBase=/share/{id}/media`，audioToken=shareId）
//! - `GET /share/{id}/media/{item}/*` 限目录文件服务（ServeDir：防穿越 + Range + MIME）
//! - `GET /share/{id}/api/local-assets/*` WE 官方素材只读代理（跟随全局设置；
//!   spike 实测不带它观感明显劣化，见方案 §6.7）
//! - `GET /props/{token}/{item}`      渲染器的 props 拉取（token=shareId，item 必须匹配）
//!
//! 到期/停用：懒校验（每次访问查表）+ 定时清扫（init 里 spawn）。

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rusqlite::Connection;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio_stream::StreamExt;
use tower::ServiceExt;
use tower_http::services::ServeDir;

use crate::db;

/// 分享子开关的设置键（默认关：分享暴露的是内容，用才开）
pub(crate) const SETTING_SHARE_ENABLED: &str = "share.enabled";

const SETTING_WE_ASSETS_DIR: &str = "wallpaper_we_assets_dir";
const SETTING_LOCAL_ASSETS: &str = "wallpaper_local_assets";

// ---------------------------------------------------------------- 存储

#[derive(Clone, Debug)]
pub(crate) struct ShareRow {
    pub id: String,
    pub item_id: String,
    pub title: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub enabled: bool,
    pub note: Option<String>,
    pub views: i64,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_from(r: &rusqlite::Row) -> rusqlite::Result<ShareRow> {
    Ok(ShareRow {
        id: r.get(0)?,
        item_id: r.get(1)?,
        title: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        created_at: r.get(3)?,
        expires_at: r.get(4)?,
        enabled: r.get::<_, i64>(5)? != 0,
        note: r.get(6)?,
        views: r.get(7)?,
    })
}

const SHARE_COLS: &str =
    "s.id, s.item_id, COALESCE(l.title, ''), s.created_at, s.expires_at, s.enabled, s.note, s.views";

fn share_json(s: &ShareRow) -> Value {
    json!({
        "shareId": s.id,
        "itemId": s.item_id,
        "title": s.title,
        "createdAt": s.created_at,
        "expiresAt": s.expires_at,
        "enabled": s.enabled,
        "note": s.note,
        "views": s.views,
    })
}

fn shares_enabled(conn: &Connection) -> bool {
    db::get_setting(conn, SETTING_SHARE_ENABLED)
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false)
}

/// 访问端校验：总开关 → 存在 → 启用 → 未过期。错误直接映射 HTTP 状态。
pub(crate) fn lookup_live(app: &AppHandle, share_id: &str) -> Result<ShareRow, Response> {
    let db = app
        .try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>()
        .ok_or_else(|| simple(StatusCode::SERVICE_UNAVAILABLE, "服务未就绪"))?;
    let conn = db.lock().map_err(|_| simple(StatusCode::INTERNAL_SERVER_ERROR, "DB 锁失败"))?;
    if !shares_enabled(&conn) {
        // 子开关关闭：分享功能整体不存在（404 而非 403，不向外人暴露它存在过）
        return Err(simple(StatusCode::NOT_FOUND, "Not Found"));
    }
    let row: Option<ShareRow> = conn
        .query_row(
            &format!(
                "SELECT {SHARE_COLS} FROM shares s LEFT JOIN library_items l ON l.item_id = s.item_id WHERE s.id = ?1"
            ),
            [share_id],
            row_from,
        )
        .ok();
    let Some(row) = row else {
        return Err(simple(StatusCode::NOT_FOUND, "Not Found"));
    };
    if !row.enabled {
        return Err(simple(StatusCode::FORBIDDEN, "该分享已停用"));
    }
    if let Some(exp) = row.expires_at {
        if exp <= now() {
            // 懒过期：顺手清行，返回 Gone
            let _ = conn.execute("DELETE FROM shares WHERE id = ?1", [share_id]);
            return Err(simple(StatusCode::GONE, "该分享已过期"));
        }
    }
    Ok(row)
}

fn simple(status: StatusCode, msg: &str) -> Response {
    let text = if status == StatusCode::NOT_FOUND {
        "Not Found".to_string()
    } else {
        format!(
            "<html><body style=\"font:15px system-ui;display:grid;place-items:center;height:100vh;margin:0\">{}</body></html>",
            html_escape(msg)
        )
    };
    let ct = if status == StatusCode::NOT_FOUND {
        "text/plain; charset=utf-8"
    } else {
        "text/html; charset=utf-8"
    };
    (
        status,
        [(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static(ct),
        )],
        text,
    )
        .into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// tauri 命令与 API 共用的存储操作 ------------------------------------------------

pub(crate) fn create(
    app: &AppHandle,
    item_id: &str,
    expires_in_sec: Option<i64>,
    note: Option<&str>,
) -> Result<Value, String> {
    if item_id.trim().is_empty() {
        return Err("缺少 itemId".into());
    }
    let id = super::random_hex(16);
    let expires_at = expires_in_sec.filter(|s| *s > 0).map(|s| now() + s);
    let db = app
        .try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>()
        .ok_or("DB 未就绪")?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM library_items WHERE item_id = ?1",
            [item_id],
            |r| r.get(0),
        )
        .ok();
    if exists.is_none() {
        return Err("该壁纸不在本地库中".into());
    }
    conn.execute(
        "INSERT INTO shares (id, item_id, created_at, expires_at, enabled, note, views)
         VALUES (?1, ?2, ?3, ?4, 1, ?5, 0)",
        rusqlite::params![id, item_id, now(), expires_at, note],
    )
    .map_err(|e| e.to_string())?;
    let title: Option<String> = conn
        .query_row(
            "SELECT title FROM library_items WHERE item_id = ?1",
            [item_id],
            |r| r.get(0),
        )
        .ok();
    Ok(json!({
        "shareId": id,
        "itemId": item_id,
        "title": title,
        "createdAt": now(),
        "expiresAt": expires_at,
        "enabled": true,
        "note": note,
        "views": 0,
    }))
}

pub(crate) fn list(app: &AppHandle) -> Result<Vec<Value>, String> {
    let db = app
        .try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>()
        .ok_or("DB 未就绪")?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {SHARE_COLS} FROM shares s LEFT JOIN library_items l ON l.item_id = s.item_id
             ORDER BY s.created_at DESC"
        ))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], row_from)
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .map(|s| share_json(&s))
        .collect::<Vec<_>>();
    Ok(rows)
}

pub(crate) fn remove(app: &AppHandle, share_id: &str) -> Result<bool, String> {
    let db = app
        .try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>()
        .ok_or("DB 未就绪")?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    let n = conn
        .execute("DELETE FROM shares WHERE id = ?1", [share_id])
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

pub(crate) fn set_enabled(app: &AppHandle, share_id: &str, enabled: bool) -> Result<bool, String> {
    let db = app
        .try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>()
        .ok_or("DB 未就绪")?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    let n = conn
        .execute(
            "UPDATE shares SET enabled = ?1 WHERE id = ?2",
            rusqlite::params![if enabled { 1 } else { 0 }, share_id],
        )
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

/// 定时清扫过期分享：懒校验已保证访问侧正确性，清扫只回收 DB 行 —— 由
/// mod.rs 持 AppHandle spawn（`spawn_share_cleanup`），10 分钟一轮。

/// 浏览计数（落地页/render 命中时 +1），同 IP 5 分钟去重（P4）：
/// 内存表记 (shareId, IP) → 5 分钟桶，同桶重复访问不计数；表超 4096 项顺手清桶。
fn bump_views(app: &AppHandle, share_id: &str, peer: IpAddr) {
    static RECENT: std::sync::OnceLock<std::sync::Mutex<HashMap<(String, IpAddr), i64>>> =
        std::sync::OnceLock::new();
    let now_bucket = now() / 300;
    {
        let recent = RECENT.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        let mut m = recent.lock().unwrap();
        if m.get(&(share_id.to_string(), peer)) == Some(&now_bucket) {
            return;
        }
        m.insert((share_id.to_string(), peer), now_bucket);
        if m.len() > 4096 {
            m.retain(|_, b| *b >= now_bucket);
        }
    }
    if let Some(db) = app.try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = conn.execute(
                "UPDATE shares SET views = views + 1 WHERE id = ?1",
                [share_id],
            );
        }
    }
}

// ---------------------------------------------------------------- 路由

pub fn router() -> Router<AppHandle> {
    Router::new()
        .route("/share/{id}", get(landing))
        .route("/share/{id}/render", get(render))
        .route("/share/{id}/embed", get(render))
        .route("/share/{id}/media/{item_id}/{*path}", get(media_file))
        .route(
            "/share/{id}/api/local-assets",
            get(local_assets_probe),
        )
        .route(
            "/share/{id}/api/local-assets/we/materials/index.json",
            get(local_assets_index),
        )
        .route(
            "/share/{id}/api/local-assets/we/{*path}",
            get(local_assets_file),
        )
        // 渲染器的 props 拉取是页面同源绝对路径 /props/{token}/{item}；
        // token = shareId（不可枚举），item 必须与该分享的条目一致
        .route("/props/{token}/{item_id}", get(props_for_share))
        // 属性实时推送（P4）：宿主改 props → 推给该条目的所有分享访客
        .route("/props-events/{token}", get(props_events))
        // 作者属性定义（访客属性表单用；值仍走 /props/{token}/{item}）
        .route("/props-defs/{token}", get(props_defs))
}

// ---- 属性实时推送（SSE）----

/// item_id → 该条目分享访客的 SSE 发送端。库在 props 热更处 publish；
/// 客户端断开（Body drop）后 send 失败即被清出。
fn props_registry(
) -> &'static std::sync::Mutex<HashMap<String, Vec<tokio::sync::mpsc::Sender<String>>>> {
    static REG: std::sync::OnceLock<
        std::sync::Mutex<HashMap<String, Vec<tokio::sync::mpsc::Sender<String>>>>,
    > = std::sync::OnceLock::new();
    REG.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// 发布属性变更到某条目的全部分享访客（wire JSON 字符串）。
/// 库侧 props 热更处调用；没有订阅者时是纯内存查表，零成本。
pub(crate) fn publish_props(item_id: &str, wire_json: String) {
    let senders: Vec<tokio::sync::mpsc::Sender<String>> = {
        let mut reg = props_registry().lock().unwrap();
        let Some(list) = reg.get_mut(item_id) else { return };
        list.retain(|tx| !tx.is_closed());
        list.clone()
    };
    for tx in senders {
        let _ = tx.try_send(wire_json.clone());
    }
}

/// 作者属性定义（UI 表单渲染用；WebPropDef 已是 camelCase 序列化）
async fn props_defs(State(app): State<AppHandle>, Path(token): Path<String>) -> Response {
    let share = match lookup_live(&app, &token) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    match crate::library::item_props(app.clone(), share.item_id.clone()) {
        Ok(defs) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            )],
            serde_json::to_string(&defs).unwrap_or_else(|_| "[]".into()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            )],
            json!({ "error": e }).to_string(),
        )
            .into_response(),
    }
}

/// SSE：`event: props` 携带 wire JSON。断线的发送端由 publish 侧 retain 清出。
async fn props_events(State(app): State<AppHandle>, Path(token): Path<String>) -> Response {
    let share = match lookup_live(&app, &token) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(16);
    {
        let mut reg = props_registry().lock().unwrap();
        reg.entry(share.item_id.clone()).or_default().push(tx);
    }
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = axum::body::Body::from_stream(stream.map(|msg| {
        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(format!(
            "event: props\ndata: {msg}\n\n"
        )))
    }));
    (
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/event-stream"),
            ),
            (
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-cache"),
            ),
        ],
        body,
    )
        .into_response()
}

// ---- 渲染页 query 构造 ----

/// 渲染器 URL query 的值编码（与 wallpaper::url_encode 同规则；那边的 fn 是私有，
/// 这里独立一份保持同字符集：保留字 [A-Za-z0-9-_.~]）
fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 构造渲染页 query。复用 resolve_item_config 拿到与桌面一致的类型与入口文件，
/// 再把绝对 src（http://127.0.0.1:port/<web|media>/<token>/<item>/…）改写成
/// 分享相对路径 /share/{id}/media/{item}/…（web 与 media 在分享侧同一条路由，
/// 因为 web 的 index.html 也在条目目录里）。
fn render_query(app: &AppHandle, share: &ShareRow) -> Result<String, Response> {
    let cfg = crate::wallpaper::resolve_item_config(app, &share.item_id)
        .map_err(|e| simple(StatusCode::NOT_FOUND, &format!("壁纸无法渲染：{e}")))?;
    let mut parts: Vec<String> = vec![format!("type={}", url_encode(&cfg.r#type))];
    match cfg.r#type.as_str() {
        // scene 的 src 就是 item_id，pkg 经 mediaBase/{itemId}/ 拉取
        "scene" => parts.push(format!("src={}", url_encode(&share.item_id))),
        _ => {
            // refresh_src 剥掉 token 后的 rel 自带 item_id 段（<item>/<rest>），
            // 这里只补 /media 前缀 —— 别把 item_id 再拼一遍（之前漏了 /media
            // 还重复了 item_id，网页壁纸的入口与资源全部 404）
            let src = cfg.src.clone().unwrap_or_default();
            let marker = if cfg.r#type == "web" { "/web/" } else { "/media/" };
            let rel = src
                .find(marker)
                .and_then(|i| src[i + marker.len()..].splitn(2, '/').nth(1))
                .ok_or_else(|| simple(StatusCode::NOT_FOUND, "壁纸入口文件无法定位"))?;
            parts.push(format!(
                "src={}",
                url_encode(&format!("/share/{}/media/{}", share.id, rel))
            ));
        }
    }
    parts.push(format!("fit={}", url_encode(&cfg.fit)));
    parts.push(format!(
        "mediaBase={}",
        url_encode(&format!("/share/{}/media", share.id))
    ));
    if let Some(pkg) = &cfg.scene_pkg {
        parts.push(format!("scenePkg={}", url_encode(pkg)));
    }
    // 官方素材跟随全局设置（spike：不带参观感明显色块化，见方案 §6.7）
    let local_assets = (|| -> Option<bool> {
        let db = app.try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>()?;
        let conn = db.lock().ok()?;
        Some(
            db::get_setting(&conn, SETTING_LOCAL_ASSETS)
                .map(|v| v != "0")
                .unwrap_or(true),
        )
    })()
    .unwrap_or(true);
    if local_assets {
        parts.push("localAssets=1".into());
    }
    // props 拉取令牌 = shareId（渲染器把 audioToken 同时用作 props token）
    parts.push(format!("audioToken={}", url_encode(&share.id)));
    Ok(format!("?{}", parts.join("&")))
}

async fn landing(
    State(app): State<AppHandle>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Path(share_id): Path<String>,
) -> Response {
    let share = match lookup_live(&app, &share_id) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    bump_views(&app, &share_id, peer.ip());
    let preview = format!("/share/{}/media/{}/preview.jpg", share.id, share.item_id);
    let preview_gif = format!("/share/{}/media/{}/preview.gif", share.id, share.item_id);
    let render_url = format!("/share/{}/render", share.id);
    let title = html_escape(if share.title.is_empty() {
        "壁纸分享"
    } else {
        &share.title
    });
    let note = share
        .note
        .as_deref()
        .map(|n| format!("<p class=\"note\">{}</p>", html_escape(n)))
        .unwrap_or_default();
    let html = format!(
        r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title} · WallpaperEM 分享</title>
<style>
body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#0d1117;color:#e6edf3;
font:15px/1.7 ui-sans-serif,system-ui,-apple-system,sans-serif}}
.card{{width:min(680px,92vw);text-align:center}}
.cover{{width:100%;aspect-ratio:16/9;object-fit:cover;border-radius:14px;border:1px solid #30363d;background:#000}}
h1{{font-size:20px;margin:18px 0 6px}}
.note{{color:#8b949e;font-size:13px}}
.open{{display:inline-block;margin:18px 0 8px;padding:10px 34px;border-radius:10px;background:#1f6feb;
color:#fff;text-decoration:none;font-size:15px;border:0;cursor:pointer}}
.open:hover{{background:#388bfd}}
.tip{{color:#8b949e;font-size:12px}}
</style></head><body>
<div class="card">
  <img class="cover" src="{preview}" onerror="this.onerror=()=>this.style.display='none';this.src='{preview_gif}'" alt="">
  <h1>{title}</h1>
  {note}
  <a class="open" href="{render_url}">打开壁纸</a>
  <p class="tip">加载需要一点时间（首次打开会下载壁纸资源）· 壁纸在本页内渲染，不影响分享者</p>
</div>
<script>
// 访客在渲染页选过横竖屏偏好 → 打开链接自动带上（localStorage 属访客本地，不回传）
try {{
  var o = localStorage.getItem("wpem.share.orient");
  if (o && o !== "auto") {{
    var a = document.querySelector(".open");
    if (a) {{ a.href = a.href + (a.href.indexOf("?") < 0 ? "?" : "&") + "orient=" + o; }}
  }}
}} catch (e) {{}}
</script></body></html>"#
    );
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
        )],
        html,
    )
        .into_response()
}

/// 直链/iframe：302 到渲染页（同源跳转，query 全保留）。
/// `/embed` 是同一处理（iframe 语义化别名）。
async fn render(
    State(app): State<AppHandle>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Path(share_id): Path<String>,
    uri: Uri,
) -> Response {
    let share = match lookup_live(&app, &share_id) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    bump_views(&app, &share_id, peer.ip());
    let query = match render_query(&app, &share) {
        Ok(q) => q,
        Err(resp) => return resp,
    };
    let _ = uri;
    match Uri::builder()
        .path_and_query(format!("/renderer/index.html{query}"))
        .build()
    {
        Ok(target) => (
            StatusCode::FOUND,
            [(
                axum::http::header::LOCATION,
                axum::http::HeaderValue::from_str(&target.to_string())
                    .unwrap_or_else(|_| axum::http::HeaderValue::from_static("/")),
            )],
        )
            .into_response(),
        Err(_) => simple(StatusCode::INTERNAL_SERVER_ERROR, "跳转构造失败"),
    }
}

/// 限目录文件服务：/share/{id}/media/{item}/{path} → 该条目目录。
/// ServeDir 自带路径穿越防护 / Range（视频拖动进度条靠它）/ 按扩展名给 MIME。
///
/// 文件段从**原始 URI**截取（仍是百分号编码）：axum 的 Path 提取会把 %20 解成
/// 字面空格，后面重建转发 URI 时就构造不出合法 Uri（400）——视频文件名带空格
/// 直接挂掉就是这个原因。ServeDir 吃的就是编码路径，内部自行解码。
async fn media_file(
    State(app): State<AppHandle>,
    // 第三个捕获组（文件段）从原始 URI 截取，这里只占位满足 axum 的全捕获要求
    Path((share_id, item_id, _path)): Path<(String, String, String)>,
    req: Request,
) -> Response {
    let share = match lookup_live(&app, &share_id) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    if item_id != share.item_id {
        return simple(StatusCode::NOT_FOUND, "Not Found");
    }
    let dir = match crate::library::item_dir(&app, &share.item_id) {
        Ok(d) => d,
        Err(e) => return simple(StatusCode::NOT_FOUND, &format!("壁纸文件不存在：{e}")),
    };
    let Some(file_path) = raw_file_path(req.uri().path(), &format!("/share/{share_id}/media/{item_id}/"))
    else {
        return simple(StatusCode::NOT_FOUND, "Not Found");
    };
    // HTML 文件必须注入 WE shim：同源入口下库期望宿主已注入（内容服务器的 /web/
    // 路由同款），裸 HTML 会让 wallpaperPropertyListener 类网页壁纸退化成默认值
    let lower = file_path.to_ascii_lowercase();
    if lower.ends_with(".html") || lower.ends_with(".htm") {
        let file = dir.join(percent_decode_path(&file_path));
        if file.is_file() {
            let html = std::fs::read(&file).unwrap_or_default();
            let injected = inject_share_html(&app, &share.item_id, html);
            return (
                StatusCode::OK,
                [(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
                )],
                injected,
            )
                .into_response();
        }
        return simple(StatusCode::NOT_FOUND, "Not Found");
    }
    serve_from(dir, file_path.to_string(), req).await
}

/// 从原始（仍编码的）URI path 里剥掉已知前缀拿文件段；不含 query。
/// 拿不到 = 路径与前缀对不上（不该发生，防御）。
fn raw_file_path<'a>(raw: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = raw.strip_prefix(prefix)?;
    Some(rest)
}

/// 分享侧的 shim 注入（复用内容服务器的核心实现）
fn inject_share_html(app: &AppHandle, item_id: &str, html: Vec<u8>) -> Vec<u8> {
    let dir = crate::library::wallpapers_dir(app).unwrap_or_default();
    if let Some(db) = app.try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>() {
        return crate::content_server::inject_we_shim_core(&db, &dir, item_id, html);
    }
    html
}

/// WE 官方素材探测（渲染器挂载 localAssets 时的第一个请求）
async fn local_assets_probe(
    State(app): State<AppHandle>,
    Path(share_id): Path<String>,
) -> Response {
    if let Err(resp) = lookup_live(&app, &share_id) {
        return resp;
    }
    let root = we_assets_root(&app);
    let body = match root {
        Some(_) => json!({ "ok": true, "roots": [ { "id": "we" } ] }),
        None => json!({ "ok": false, "roots": [] }),
    };
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )],
        body.to_string(),
    )
        .into_response()
}

async fn local_assets_index(
    State(app): State<AppHandle>,
    Path(share_id): Path<String>,
) -> Response {
    if let Err(resp) = lookup_live(&app, &share_id) {
        return resp;
    }
    let Some(root) = we_assets_root(&app) else {
        return simple(StatusCode::NOT_FOUND, "Not Found");
    };
    let names = crate::we_assets::material_names(&root);
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )],
        json!({ "names": names }).to_string(),
    )
        .into_response()
}

async fn local_assets_file(
    State(app): State<AppHandle>,
    Path((share_id, _path)): Path<(String, String)>,
    req: Request,
) -> Response {
    if let Err(resp) = lookup_live(&app, &share_id) {
        return resp;
    }
    let Some(root) = we_assets_root(&app) else {
        return simple(StatusCode::NOT_FOUND, "Not Found");
    };
    // 文件段取原始编码路径（素材文件名也可能带空格/中文，见 media_file 注释）
    let Some(file_path) =
        raw_file_path(req.uri().path(), &format!("/share/{share_id}/api/local-assets/we/"))
    else {
        return simple(StatusCode::NOT_FOUND, "Not Found");
    };
    serve_from(root, file_path.to_string(), req).await
}

fn we_assets_root(app: &AppHandle) -> Option<std::path::PathBuf> {
    let custom = {
        let db = app.try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>()?;
        db.lock().ok().and_then(|conn| {
            db::get_setting(&conn, SETTING_WE_ASSETS_DIR)
        })
    };
    crate::we_assets::resolve_root(custom.as_deref())
}

/// 把请求 URI 重写成相对路径后交给 ServeDir（限目录 + 防穿越在它内部）。
/// mod.rs 的 prod 渲染器静态服务也走这里（同一套重写 + 兜底）。
pub(crate) async fn serve_from(dir: std::path::PathBuf, path: String, req: Request) -> Response {
    let rel = format!("/{}", path.trim_start_matches('/'));
    let (mut parts, body) = req.into_parts();
    match Uri::builder().path_and_query(rel).build() {
        Ok(uri) => parts.uri = uri,
        Err(_) => return simple(StatusCode::BAD_REQUEST, "Bad Request"),
    }
    match ServeDir::new(dir).oneshot(axum::http::Request::from_parts(parts, body)).await {
        Ok(resp) => {
            let mut resp = resp.into_response();
            // 库文件不可变：允许浏览器缓存 —— 分享渲染页据此把 scene.pkg 预取进
            // HTTP 缓存，库随后的同 URL 请求秒回缓存（否则双倍下载）。注入过 shim
            // 的 HTML 走手工分支不带这个头（含种子数据，保守不缓存）
            resp.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("public, max-age=86400"),
            );
            resp
        }
        Err(e) => {
            tracing::warn!("share file serve failed: {e}");
            simple(StatusCode::INTERNAL_SERVER_ERROR, "文件服务失败")
        }
    }
}

/// 渲染器的 props 拉取：token 必须是活着的分享 id，item 必须匹配该分享
async fn props_for_share(
    State(app): State<AppHandle>,
    Path((token, item_id)): Path<(String, String)>,
) -> Response {
    let share = match lookup_live(&app, &token) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let item_id = percent_decode(&item_id);
    if item_id != share.item_id {
        return simple(StatusCode::NOT_FOUND, "Not Found");
    }
    let props = (|| -> Option<serde_json::Map<String, Value>> {
        let db = app.try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>()?;
        let conn = db.lock().ok()?;
        Some(crate::we_props::effective_props(
            &conn,
            &crate::library::wallpapers_dir(&app).unwrap_or_default(),
            &item_id,
        ))
    })()
    .unwrap_or_default();
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )],
        Value::Object(props).to_string(),
    )
        .into_response()
}

fn percent_decode(s: &str) -> String {
    percent_decode_impl(s, true)
}

/// 路径版解码：`+` 是合法文件名字符，**不**当空格（与表单语义区分开）
fn percent_decode_path(s: &str) -> String {
    percent_decode_impl(s, false)
}

fn percent_decode_impl(s: &str, plus_as_space: bool) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 2 <= bytes.len() - 1 {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' && plus_as_space {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------- tauri 命令

#[tauri::command(rename = "share_list")]
pub fn share_list_cmd(app: AppHandle) -> Result<Vec<Value>, String> {
    list(&app)
}

#[tauri::command(rename = "share_create")]
pub fn share_create_cmd(
    app: AppHandle,
    item_id: String,
    expires_in_sec: Option<i64>,
    note: Option<String>,
) -> Result<Value, String> {
    create(&app, &item_id, expires_in_sec, note.as_deref())
}

#[tauri::command(rename = "share_remove")]
pub fn share_remove_cmd(app: AppHandle, share_id: String) -> Result<bool, String> {
    remove(&app, &share_id)
}

#[tauri::command(rename = "share_set_enabled")]
pub fn share_set_enabled_cmd(
    app: AppHandle,
    share_id: String,
    enabled: bool,
) -> Result<bool, String> {
    set_enabled(&app, &share_id, enabled)
}

/// 读分享子开关（设置页渲染用）
#[tauri::command(rename = "share_enabled_status")]
pub fn share_enabled_status_cmd(app: AppHandle) -> bool {
    app.try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>()
        .and_then(|db| db.lock().ok().map(|conn| shares_enabled(&conn)))
        .unwrap_or(false)
}

/// 写分享子开关（落 DB + 广播；分享路由每个请求现读，无需重启）
#[tauri::command(rename = "share_set_service_enabled")]
pub fn share_set_service_enabled_cmd(app: AppHandle, enabled: bool) -> Result<(), String> {
    let db = app
        .try_state::<std::sync::Arc<std::sync::Mutex<Connection>>>()
        .ok_or("DB 未就绪")?;
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::set_setting(&conn, SETTING_SHARE_ENABLED, if enabled { "1" } else { "0" })?;
    }
    crate::notify_setting_changed(&app, SETTING_SHARE_ENABLED, if enabled { "1" } else { "0" });
    Ok(())
}
