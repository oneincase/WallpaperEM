//! REST API（/api/v1/*）：给脚本 / 快捷指令 / 自动化的标准控制接口。
//!
//! 与 MCP 工具共享同一个命令层 —— 每个端点都是 [`crate::mcp::tools::call`] 的
//! 薄封装，不重写业务逻辑；MCP 与 REST 是同一能力的两种编码。
//! 鉴权与网络模式防护与 /mcp 完全一致（token + 按模式的 Origin 策略 + 局域网
//! 源过滤，见 [`super::authorize`] 与 `net_guard` 层）。`/docs` 与
//! `/api/v1/openapi.json` 是描述文档，无敏感数据，不要求 token。

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde_json::{json, Value};
use tauri::AppHandle;

use super::{authorize, json_response, McpState};
use crate::mcp::tools;

pub fn router() -> Router<AppHandle> {
    Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/displays", get(displays))
        .route("/api/v1/library", get(library))
        .route("/api/v1/library/{item_id}", get(library_item))
        .route("/api/v1/apply", post(apply))
        .route("/api/v1/stop", post(stop))
        .route("/api/v1/pause", post(pause))
        .route("/api/v1/resume", post(resume))
        .route("/api/v1/next", post(next))
        .route("/api/v1/prev", post(prev))
        .route("/api/v1/playlists", get(playlists))
        .route("/api/v1/playlists/{id}/apply", post(playlist_apply))
        .route("/api/v1/settings/{key}", get(setting_get).put(setting_put))
        .route("/api/v1/screenshot/{item_id}", get(screenshot))
        .route("/api/v1/shares", get(shares_list).post(share_create))
        .route("/api/v1/shares/{share_id}", delete(share_delete))
        .route("/api/v1/shares/{share_id}/enabled", put(share_enabled))
        .route("/api/v1/openapi.json", get(openapi))
}

use axum::routing::{delete, post};

// ---------------------------------------------------------------- 胶水

/// 调 MCP 工具层并解包：text content 里的 JSON 就是端点响应体；is_err → 4xx。
/// 错误文案含「不存在 / 没有」时升为 404，其余 400。
async fn tool_json(
    app: &AppHandle,
    name: &str,
    args: Value,
) -> Result<Value, (StatusCode, Value)> {
    let (blocks, is_err) = tools::call(app, name, &args).await;
    let text = blocks
        .first()
        .and_then(|b| b.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let v: Value = serde_json::from_str(text).unwrap_or_else(|_| json!({ "raw": text }));
    if is_err {
        let msg = v.get("error").and_then(|e| e.as_str()).unwrap_or("");
        let status = if msg.contains("不存在") || msg.contains("没有条目") {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        return Err((status, v));
    }
    Ok(v)
}

async fn tool(app: &AppHandle, name: &str, args: Value) -> Response {
    match tool_json(app, name, args).await {
        Ok(v) => json_response(StatusCode::OK, &v),
        Err((status, v)) => json_response(status, &v),
    }
}

/// token 鉴权（Bearer 或 ?token=），复用 /mcp 的 authorize（含按模式的 Origin 策略）。
fn authorized(app: &AppHandle, headers: &HeaderMap, query: &HashMap<String, String>) -> Option<Response> {
    let Some(st) = app.try_state::<McpState>() else {
        return Some(json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({ "error": "服务未就绪" }),
        ));
    };
    match authorize(&st, headers, query) {
        Ok(()) => None,
        Err(resp) => Some(resp),
    }
}

use std::collections::HashMap;
use tauri::Manager;

/// MCP text-content → 纯 JSON 响应之外，截图这类带图片块的端点自己取 blocks。
async fn tool_raw(
    app: &AppHandle,
    name: &str,
    args: Value,
) -> (Value, Option<(String, String)>, bool) {
    let (blocks, is_err) = tools::call(app, name, &args).await;
    let text = blocks
        .first()
        .and_then(|b| b.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let v: Value = serde_json::from_str(text).unwrap_or_else(|_| json!({ "raw": text }));
    let image = blocks.first().and_then(|b| {
        let data = b.get("data")?.as_str()?.to_string();
        let mime = b.get("mimeType")?.as_str()?.to_string();
        Some((data, mime))
    });
    (v, image, is_err)
}

// ---------------------------------------------------------------- 端点

async fn status(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    let sessions = tool_json(&app, "list_sessions", json!({})).await;
    let active = tool_json(&app, "active_items", json!({})).await;
    let rotation = tool_json(&app, "playlist_status", json!({})).await;
    let body = json!({
        "app": "WallpaperEM",
        "version": app.package_info().version.to_string(),
        "sessions": sessions.ok(),
        "activeItems": active.ok(),
        "rotation": rotation.ok(),
    });
    json_response(StatusCode::OK, &body)
}

async fn displays(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    tool(&app, "displays_list", json!({})).await
}

async fn library(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    let args = json!({
        "query": query.get("query"),
        "type": query.get("type"),
        "limit": query.get("limit").and_then(|v| v.parse::<u64>().ok()),
        "offset": query.get("offset").and_then(|v| v.parse::<u64>().ok()),
    });
    tool(&app, "library_list", args).await
}

async fn library_item(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(item_id): Path<String>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    match tools::library_item_json(&app, &item_id) {
        Ok(v) => json_response(StatusCode::OK, &v),
        Err(e) => json_response(StatusCode::NOT_FOUND, &json!({ "error": e })),
    }
}

async fn apply(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    let Some(item_id) = body.get("itemId").and_then(|v| v.as_str()) else {
        return json_response(StatusCode::BAD_REQUEST, &json!({ "error": "缺少 itemId" }));
    };
    let args = json!({ "itemId": item_id, "displayId": body.get("displayId") });
    tool(&app, "wallpaper_apply", args).await
}

macro_rules! simple_action {
    ($fn_name:ident, $tool_name:literal, $ok:expr) => {
        async fn $fn_name(
            State(app): State<AppHandle>,
            headers: HeaderMap,
            Query(query): Query<HashMap<String, String>>,
        ) -> Response {
            if let Some(resp) = authorized(&app, &headers, &query) {
                return resp;
            }
            tool(&app, $tool_name, json!({})).await
        }
    };
}

simple_action!(stop, "wallpaper_stop", "stopped");
simple_action!(pause, "wallpaper_pause", "paused");
simple_action!(resume, "wallpaper_resume", "resumed");

async fn next(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    tool(&app, "wallpaper_next", json!({})).await
}

async fn prev(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    tool(&app, "wallpaper_prev", json!({})).await
}

async fn playlists(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    tool(&app, "playlist_list", json!({})).await
}

async fn playlist_apply(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<i64>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    tool(&app, "playlist_apply", json!({ "id": id })).await
}

// ---- 设置读写：白名单（渲染类共享设置），写走与设置页/托盘同一批命令 ----

const SETTING_KEYS: &[&str] = &[
    "wallpaper_fit",
    "wallpaper_render_dpr",
    "wallpaper_scene_fps",
    "wallpaper_reveal",
    "wallpaper_aa",
    "wallpaper_particles",
    "wallpaper_post",
];

async fn setting_get(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(key): Path<String>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    if !SETTING_KEYS.contains(&key.as_str()) {
        return json_response(
            StatusCode::NOT_FOUND,
            &json!({ "error": format!("不支持的设置键: {key}（可读写: {}）", SETTING_KEYS.join("/")) }),
        );
    }
    let value = read_setting(&app, &key);
    json_response(StatusCode::OK, &json!({ "key": key, "value": value }))
}

/// 读设置键的现值（无值返回 null；DB 未就绪也返回 null，读操作不该 5xx）
fn read_setting(app: &AppHandle, key: &str) -> Value {
    (|| -> Option<Value> {
        let db = app.try_state::<std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>>()?;
        let conn = db.lock().ok()?;
        crate::db::get_setting(&conn, key).map(Value::String)
    })()
    .unwrap_or(Value::Null)
}

async fn setting_put(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(key): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    if !SETTING_KEYS.contains(&key.as_str()) {
        return json_response(
            StatusCode::NOT_FOUND,
            &json!({ "error": format!("不支持的设置键: {key}（可读写: {}）", SETTING_KEYS.join("/")) }),
        );
    }
    let Some(value) = body.get("value").and_then(|v| v.as_str()) else {
        return json_response(StatusCode::BAD_REQUEST, &json!({ "error": "缺少 value（字符串）" }));
    };
    let res = match key.as_str() {
        "wallpaper_fit" => crate::wallpaper::set_fit(app.clone(), value.to_string()),
        "wallpaper_render_dpr" => value.parse::<f32>().map_err(|e| e.to_string()).and_then(|d| {
            crate::wallpaper::set_render_dpr(app.clone(), d)
        }),
        "wallpaper_scene_fps" => value.parse::<u32>().map_err(|e| e.to_string()).and_then(|f| {
            crate::wallpaper::set_scene_fps(app.clone(), f)
        }),
        "wallpaper_reveal" => crate::wallpaper::set_reveal(app.clone(), value.to_string()),
        "wallpaper_aa" => crate::wallpaper::set_aa(app.clone(), value.to_string()),
        "wallpaper_particles" => crate::wallpaper::set_particles(app.clone(), value.to_string()),
        "wallpaper_post" => crate::wallpaper::set_post(app.clone(), value.to_string()),
        _ => unreachable!("白名单已校验"),
    };
    match res {
        Ok(()) => json_response(StatusCode::OK, &json!({ "key": key, "value": value })),
        Err(e) => json_response(StatusCode::BAD_REQUEST, &json!({ "error": e })),
    }
}

async fn screenshot(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(item_id): Path<String>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    let (v, image, is_err) = tool_raw(&app, "wallpaper_screenshot", json!({ "itemId": item_id })).await;
    if is_err {
        return json_response(StatusCode::BAD_REQUEST, &v);
    }
    match image {
        Some((data, mime)) => json_response(
            StatusCode::OK,
            &json!({ "itemId": item_id, "mimeType": mime, "base64": data }),
        ),
        None => json_response(StatusCode::OK, &v),
    }
}

// ---- 分享（P3；存储与校验在 shares.rs）----

async fn shares_list(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    match super::shares::list(&app) {
        Ok(v) => json_response(StatusCode::OK, &json!({ "items": v })),
        Err(e) => json_response(StatusCode::INTERNAL_SERVER_ERROR, &json!({ "error": e })),
    }
}

async fn share_create(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    let item_id = body.get("itemId").and_then(|v| v.as_str()).unwrap_or("");
    let expires_in = body.get("expiresInSec").and_then(|v| v.as_i64());
    let note = body.get("note").and_then(|v| v.as_str());
    match super::shares::create(&app, item_id, expires_in, note) {
        Ok(v) => json_response(StatusCode::OK, &v),
        Err(e) if e.contains("不在本地库") => json_response(StatusCode::NOT_FOUND, &json!({ "error": e })),
        Err(e) => json_response(StatusCode::BAD_REQUEST, &json!({ "error": e })),
    }
}

async fn share_delete(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(share_id): Path<String>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    match super::shares::remove(&app, &share_id) {
        Ok(true) => json_response(StatusCode::OK, &json!({ "deleted": true })),
        Ok(false) => json_response(StatusCode::NOT_FOUND, &json!({ "deleted": false })),
        Err(e) => json_response(StatusCode::INTERNAL_SERVER_ERROR, &json!({ "error": e })),
    }
}

async fn share_enabled(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(share_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(resp) = authorized(&app, &headers, &query) {
        return resp;
    }
    let Some(enabled) = body.get("enabled").and_then(|v| v.as_bool()) else {
        return json_response(StatusCode::BAD_REQUEST, &json!({ "error": "缺少 enabled（布尔）" }));
    };
    match super::shares::set_enabled(&app, &share_id, enabled) {
        Ok(true) => json_response(StatusCode::OK, &json!({ "shareId": share_id, "enabled": enabled })),
        Ok(false) => json_response(StatusCode::NOT_FOUND, &json!({ "error": "分享不存在" })),
        Err(e) => json_response(StatusCode::INTERNAL_SERVER_ERROR, &json!({ "error": e })),
    }
}

// ---------------------------------------------------------------- 文档

/// OpenAPI 3.1 描述。端点与上面一一对应；改端点时同步这份（人工维护，宁缺勿错）。
async fn openapi(State(app): State<AppHandle>) -> Response {
    let base = "/api/v1";
    let ok = |desc: &str| json!({ "description": desc });
    let json_body = |props: Value, required: &[&str]| {
        json!({
            "required": required,
            "content": { "application/json": { "schema": { "type": "object", "properties": props } } }
        })
    };
    let mut paths = serde_json::Map::new();
    let mut p = |path: &str, item: Value| {
        paths.insert(format!("{base}{path}"), item);
    };
    p(
        "/status",
        json!({ "get": { "summary": "运行状态（版本/会话/轮播）", "responses": { "200": ok("状态对象") } } }),
    );
    p(
        "/displays",
        json!({ "get": { "summary": "显示器与每屏会话", "responses": { "200": ok("显示器列表") } } }),
    );
    p(
        "/library",
        json!({ "get": {
            "summary": "本地库列表",
            "parameters": [
                { "name": "query", "in": "query", "schema": { "type": "string" } },
                { "name": "type", "in": "query", "schema": { "type": "string" } },
                { "name": "limit", "in": "query", "schema": { "type": "integer", "maximum": 500 } },
                { "name": "offset", "in": "query", "schema": { "type": "integer" } },
            ],
            "responses": { "200": ok("分页列表") }
        } }),
    );
    p(
        "/library/{itemId}",
        json!({ "get": { "summary": "本地库条目详情", "responses": { "200": ok("条目"), "404": ok("不存在") } } }),
    );
    p(
        "/apply",
        json!({ "post": {
            "summary": "应用壁纸",
            "requestBody": json_body(json!({
                "itemId": { "type": "string" },
                "displayId": { "type": "string", "description": "缺省 = 全部显示器" }
            }), &["itemId"]),
            "responses": { "200": ok("已应用"), "400": ok("参数错误"), "404": ok("条目不存在") }
        } }),
    );
    for (path, summary) in [
        ("/stop", "停止壁纸"),
        ("/pause", "暂停渲染"),
        ("/resume", "恢复渲染"),
        ("/next", "下一张"),
        ("/prev", "上一张"),
    ] {
        p(path, json!({ "post": { "summary": summary, "responses": { "200": ok("结果") } } }));
    }
    p(
        "/playlists",
        json!({ "get": { "summary": "轮播列表", "responses": { "200": ok("列表") } } }),
    );
    p(
        "/playlists/{id}/apply",
        json!({ "post": { "summary": "激活轮播", "responses": { "200": ok("结果"), "404": ok("不存在") } } }),
    );
    p(
        "/settings/{key}",
        json!({
            "get": { "summary": "读设置（白名单：fit/renderDpr/sceneFps/reveal/aa/particles/post）", "responses": { "200": ok("键值"), "404": ok("键不在白名单") } },
            "put": {
                "summary": "写设置（同一白名单，写路径与设置页/托盘共用）",
                "requestBody": json_body(json!({ "value": { "type": "string" } }), &["value"]),
                "responses": { "200": ok("已写入"), "400": ok("值非法"), "404": ok("键不在白名单") }
            }
        }),
    );
    p(
        "/screenshot/{itemId}",
        json!({ "get": { "summary": "实拍截图（base64 JPEG）", "responses": { "200": ok("图片数据"), "400": ok("截图失败") } } }),
    );
    p(
        "/shares",
        json!({
            "get": { "summary": "我的分享", "responses": { "200": ok("列表") } },
            "post": {
                "summary": "创建分享",
                "requestBody": json_body(json!({
                    "itemId": { "type": "string" },
                    "expiresInSec": { "type": "integer", "description": "缺省 = 永久" },
                    "note": { "type": "string" }
                }), &["itemId"]),
                "responses": { "200": ok("分享对象"), "404": ok("条目不在本地库") }
            }
        }),
    );
    p(
        "/shares/{shareId}",
        json!({ "delete": { "summary": "删除分享", "responses": { "200": ok("已删"), "404": ok("不存在") } } }),
    );
    p(
        "/shares/{shareId}/enabled",
        json!({
            "put": {
                "summary": "启停分享",
                "requestBody": json_body(json!({ "enabled": { "type": "boolean" } }), &["enabled"]),
                "responses": { "200": ok("已更新"), "404": ok("不存在") }
            }
        }),
    );
    let body = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "WallpaperEM API",
            "version": app.package_info().version.to_string(),
            "description": "WallpaperEM 网络服务 REST 接口。鉴权：Authorization: Bearer <token> 或 ?token=<token>（token 在应用 设置 → 网络与服务 中查看）。/docs 有带示例的人类可读版本。"
        },
        "servers": [ { "url": "/" } ],
        "paths": paths,
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// 人类可读文档页（离线内联，不引 CDN；有 openapi.json 在，这页只求可读）。
pub async fn docs_page() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        DOCS_HTML,
    )
        .into_response()
}

const DOCS_HTML: &str = r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>WallpaperEM · API 文档</title>
<style>
body{margin:0;background:#0d1117;color:#e6edf3;font:14px/1.7 ui-sans-serif,system-ui,-apple-system,sans-serif}
main{max-width:860px;margin:0 auto;padding:32px 20px 80px}
h1{font-size:22px}h2{font-size:16px;margin:32px 0 8px;color:#79c0ff}
code,pre{font:12.5px ui-monospace,SFMono-Regular,Menlo,monospace}
pre{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:12px 14px;overflow:auto}
table{border-collapse:collapse;width:100%;margin:8px 0}
th,td{border:1px solid #30363d;padding:6px 10px;text-align:left;vertical-align:top}
th{background:#161b22}code.m{color:#7ee787}a{color:#58a6ff}
.note{color:#8b949e;font-size:12.5px}
</style></head><body><main>
<h1>WallpaperEM REST API <span class="note">/api/v1</span></h1>
<p>控制桌面壁纸：应用 / 暂停 / 下一张 / 查库 / 改设置 / 管分享。机器可读描述见
<a href="/api/v1/openapi.json">openapi.json</a>（可导入 Apifox / Postman / Insomnia）。</p>
<h2>鉴权</h2>
<pre>Authorization: Bearer &lt;token&gt;        # 推荐
?token=&lt;token&gt;                      # 兼容</pre>
<p class="note">token 在应用「设置 → 网络与服务」查看/轮换。可访问范围随网络模式变化：
本机=仅 127.0.0.1，局域网=同网设备（公网来源一律拒绝），任意=不限来源。</p>
<h2>端点</h2>
<table>
<tr><th>方法</th><th>路径</th><th>说明</th></tr>
<tr><td><code class="m">GET</code></td><td>/api/v1/status</td><td>运行状态（版本/会话/轮播）</td></tr>
<tr><td><code class="m">GET</code></td><td>/api/v1/displays</td><td>显示器与每屏会话</td></tr>
<tr><td><code class="m">GET</code></td><td>/api/v1/library</td><td>本地库（query/type/limit/offset）</td></tr>
<tr><td><code class="m">GET</code></td><td>/api/v1/library/{itemId}</td><td>条目详情</td></tr>
<tr><td><code class="m">POST</code></td><td>/api/v1/apply</td><td>应用壁纸 <code>{"itemId":"…","displayId":"可选"}</code></td></tr>
<tr><td><code class="m">POST</code></td><td>/api/v1/stop · /pause · /resume · /next · /prev</td><td>播放控制</td></tr>
<tr><td><code class="m">GET</code></td><td>/api/v1/playlists</td><td>轮播列表</td></tr>
<tr><td><code class="m">POST</code></td><td>/api/v1/playlists/{id}/apply</td><td>激活轮播</td></tr>
<tr><td><code class="m">GET/PUT</code></td><td>/api/v1/settings/{key}</td><td>设置读写；键白名单：fit / renderDpr / sceneFps / reveal / aa / particles / post（PUT 体 <code>{"value":"…"}</code>）</td></tr>
<tr><td><code class="m">GET</code></td><td>/api/v1/screenshot/{itemId}</td><td>实拍截图（base64）</td></tr>
<tr><td><code class="m">GET/POST</code></td><td>/api/v1/shares</td><td>分享列表 / 创建（itemId、expiresInSec 缺省=永久、note）</td></tr>
<tr><td><code class="m">DELETE</code></td><td>/api/v1/shares/{shareId}</td><td>删除分享</td></tr>
<tr><td><code class="m">PUT</code></td><td>/api/v1/shares/{shareId}/enabled</td><td>启停分享 <code>{"enabled":true}</code></td></tr>
</table>
<h2>示例</h2>
<pre># 看状态
curl -H "Authorization: Bearer $TOKEN" http://192.168.1.2:7411/api/v1/status
# 换壁纸
curl -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -X POST http://192.168.1.2:7411/api/v1/apply -d '{"itemId":"3293156956"}'
# 下一张
curl -H "Authorization: Bearer $TOKEN" -X POST http://192.168.1.2:7411/api/v1/next
# 改切换效果
curl -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -X PUT http://192.168.1.2:7411/api/v1/settings/wallpaper_reveal -d '{"value":"circle"}'
# 创建 7 天分享
curl -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -X POST http://192.168.1.2:7411/api/v1/shares -d '{"itemId":"3293156956","expiresInSec":604800}'</pre>
<p class="note">错误统一 JSON：{"error":"…"}；401 未鉴权 / 403 来源或 Origin 被拒 / 404 不存在或键不在白名单 / 410 分享已过期。</p>
</main></body></html>"#;
