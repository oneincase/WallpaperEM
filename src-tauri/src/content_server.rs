//! 内容服务器（T3）：为壁纸渲染器/预览提供本地文件服务
//!
//! 手写 tokio TCP HTTP/1.1 文件服务（确定性、无框架魔法）：
//! - 127.0.0.1 随机端口；短时效 token + itemId 白名单（查 library_items）
//! - 路径规范化防穿越；支持 Range（视频拖拽）；MIME 按扩展名
//! - web 壁纸 iframe 只能访问自己目录内的资源

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde_json::json;
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::util::random_hex;

#[derive(Clone)]
pub struct ContentServerState {
    #[allow(dead_code)]
    pub port: u16,
    pub token: String,
    pub db: Arc<Mutex<Connection>>,
    pub wallpapers_dir: PathBuf,
    /// prod：打包进资源目录的 dist/renderer（dev 由 vite 代理）
    #[allow(dead_code)]
    pub renderer_dir: PathBuf,
}

pub fn init(app: &AppHandle) -> Result<(), String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let wallpapers_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("wallpapers");
    std::fs::create_dir_all(&wallpapers_dir).map_err(|e| e.to_string())?;

    let token = random_hex(16);
    let port_state = Arc::new(Mutex::new(0u16));
    app.manage(port_state.clone());

    let state = ContentServerState {
        port: 0,
        token,
        db: db.inner().clone(),
        wallpapers_dir,
        renderer_dir: app
            .path()
            .resource_dir()
            .map(|r| r.join("renderer"))
            .unwrap_or_default(),
    };
    app.manage(state.clone());

    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("content server bind failed: {e}");
                return;
            }
        };
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        if let Some(st) = app2.try_state::<Arc<Mutex<u16>>>() {
            *st.lock().unwrap() = port;
        }
        tracing::info!("content server listening on 127.0.0.1:{port} (token={})", state.token);

        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let state = state.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = handle_conn(&mut stream, &state).await {
                    tracing::debug!("content server conn error: {e}");
                }
            });
        }
    });
    Ok(())
}

/// 渲染器页代理/服务：与媒体同源，消除跨源 fetch 限制
#[allow(unused_variables)]
async fn proxy_renderer(
    stream: &mut tokio::net::TcpStream,
    path: &str,
    state: &ContentServerState,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        // dev：代理到 vite dev server（与 tauri.conf devUrl 一致）
        let vite_path = path.trim_start_matches('/');
        let upstream = format!("http://localhost:1420/{vite_path}");
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| e.to_string())?;
        match client.get(&upstream).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let reason = resp.status().canonical_reason().unwrap_or("OK");
                let ct = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let body = resp.bytes().await.unwrap_or_default();
                return respond(stream, status, reason, &ct, &body, None).await;
            }
            Err(e) => {
                return respond(
                    stream,
                    502,
                    "Bad Gateway",
                    "text/plain",
                    format!("vite proxy error: {e}").as_bytes(),
                    None,
                )
                .await;
            }
        }
    }
    #[cfg(not(debug_assertions))]
    {
        // prod：服务打包进资源目录的 dist/renderer
        let res = state.renderer_dir.join(path.trim_start_matches("/renderer/"));
        let file = if res.is_dir() {
            res.join("index.html")
        } else {
            res
        };
        if !file.is_file() {
            return respond(stream, 404, "Not Found", "text/plain", b"", None).await;
        }
        let data = tokio::fs::read(&file).await.map_err(|e| e.to_string())?;
        let mime = mime_for(&file);
        return respond(
            stream,
            200,
            "OK",
            mime,
            &data,
            Some(&format!("Content-Length: {}", data.len())),
        )
        .await;
    }
    #[allow(unreachable_code)]
    Ok(())
}

/// 默认壁纸页（无壁纸时的默认 HTML 壁纸）：dev → 代理 vite；prod → 资源目录
#[allow(unused_variables)]
async fn proxy_default_wallpaper(
    stream: &mut tokio::net::TcpStream,
    path: &str,
    state: &ContentServerState,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        // dev：代理到 vite dev server（public/default-wallpaper）
        let vite_path = path.trim_start_matches('/');
        let upstream = format!("http://localhost:1420/{vite_path}");
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| e.to_string())?;
        match client.get(&upstream).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let reason = resp.status().canonical_reason().unwrap_or("OK");
                let ct = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let body = resp.bytes().await.unwrap_or_default();
                return respond(stream, status, reason, &ct, &body, None).await;
            }
            Err(e) => {
                return respond(
                    stream,
                    502,
                    "Bad Gateway",
                    "text/plain",
                    format!("vite proxy error: {e}").as_bytes(),
                    None,
                )
                .await;
            }
        }
    }
    #[cfg(not(debug_assertions))]
    {
        // prod：服务打包进资源目录的 default-wallpaper
        let base = state
            .renderer_dir
            .parent()
            .unwrap_or(&state.renderer_dir)
            .join("default-wallpaper");
        let res = base.join(path.trim_start_matches("/default-wallpaper/"));
        let file = if res.is_dir() {
            res.join("index.html")
        } else {
            res
        };
        if !file.is_file() {
            return respond(stream, 404, "Not Found", "text/plain", b"", None).await;
        }
        let data = tokio::fs::read(&file).await.map_err(|e| e.to_string())?;
        let mime = mime_for(&file);
        return respond(
            stream,
            200,
            "OK",
            mime,
            &data,
            Some(&format!("Content-Length: {}", data.len())),
        )
        .await;
    }
    #[allow(unreachable_code)]
    Ok(())
}

/// 通用静态资源（assets / test-media 等顶层目录）：dev → 代理 vite；prod → 资源目录
#[allow(unused_variables)]
async fn proxy_static(
    stream: &mut tokio::net::TcpStream,
    path: &str,
    state: &ContentServerState,
    sub: &str,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        let vite_path = path.trim_start_matches('/');
        let upstream = format!("http://localhost:1420/{vite_path}");
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| e.to_string())?;
        match client.get(&upstream).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let reason = resp.status().canonical_reason().unwrap_or("OK");
                let ct = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let body = resp.bytes().await.unwrap_or_default();
                return respond(stream, status, reason, &ct, &body, None).await;
            }
            Err(e) => {
                return respond(
                    stream,
                    502,
                    "Bad Gateway",
                    "text/plain",
                    format!("vite proxy error: {e}").as_bytes(),
                    None,
                )
                .await;
            }
        }
    }
    #[cfg(not(debug_assertions))]
    {
        let base = state
            .renderer_dir
            .parent()
            .unwrap_or(&state.renderer_dir)
            .join(sub);
        let rel = path.trim_start_matches(&format!("/{sub}"));
        let rel = rel.trim_start_matches('/');
        let res = base.join(rel);
        let file = if res.is_dir() {
            res.join("index.html")
        } else {
            res
        };
        if !file.is_file() {
            return respond(stream, 404, "Not Found", "text/plain", b"", None).await;
        }
        let data = tokio::fs::read(&file).await.map_err(|e| e.to_string())?;
        let mime = mime_for(&file);
        return respond(
            stream,
            200,
            "OK",
            mime,
            &data,
            Some(&format!("Content-Length: {}", data.len())),
        )
        .await;
    }
    #[allow(unreachable_code)]
    Ok(())
}

async fn handle_conn(
    stream: &mut tokio::net::TcpStream,
    state: &ContentServerState,
) -> Result<(), String> {
    // 读取请求头（最多 16KB，直到空行）
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16384 {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("/");

    // 分离 query 与纯路径。渲染器页 URL 始终带 ?type=...&src=...&mediaBase=...，
    // 文件服务必须忽略 query（否则 prod 下会把整串当作文件名 → 404）；浏览器会保留
    // 原始 URL 的 query 供渲染器读取，故仅影响「取哪个文件」，不影响前端取参数。
    // /diag 端点仍需 query（?msg=...），因此单独保留。
    let (path, query) = match raw_path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (raw_path, ""),
    };

    // CORS 预检（渲染器 fetch 带 cache:no-store 头 → 非简单请求）
    if method == "OPTIONS" {
        return respond(
            stream,
            204,
            "No Content",
            "text/plain",
            b"",
            Some("Access-Control-Allow-Headers: *\r\nAccess-Control-Max-Age: 600"),
        )
        .await;
    }
    if method != "GET" {
        return respond(stream, 405, "Method Not Allowed", "text/plain", b"", None).await;
    }

    // 渲染器页：与媒体同源（免 CORS）。dev → 代理 vite；prod → 资源目录
    if path.starts_with("/renderer") {
        return proxy_renderer(stream, path, state).await;
    }

    // 默认壁纸页（无壁纸时的默认 HTML 壁纸）
    if path.starts_with("/default-wallpaper") {
        return proxy_default_wallpaper(stream, path, state).await;
    }

    // 渲染器/网页壁纸引用的顶层静态资源（assets / test-media）
    if path.starts_with("/assets") {
        return proxy_static(stream, path, state, "assets").await;
    }
    if path.starts_with("/test-media") {
        return proxy_static(stream, path, state, "test-media").await;
    }

    // 渲染器诊断上报端点：/diag?msg=<urlencoded>
    if path.starts_with("/diag") {
        let msg = query
            .strip_prefix("msg=")
            .map(|m| percent_decode(m))
            .unwrap_or_default();
        tracing::warn!("[renderer diag] {msg}");
        return respond(stream, 200, "OK", "text/plain", b"ok", None).await;
    }

    // 解析路径 /media/{token}/{item_id}/{path...} 或 /web/{token}/{item_id}/{path...}
    // （/web 为 web 壁纸站点根：绝对路径引用（/js/...）也能正确解析）
    // 注意：浏览器会对非 ASCII 文件名做百分号编码，各段必须先解码再匹配磁盘路径
    let segments: Vec<String> = path
        .trim_start_matches('/')
        .split('/')
        .map(percent_decode)
        .collect();
    if segments.len() < 4 || (segments[0] != "media" && segments[0] != "web") {
        return respond(stream, 404, "Not Found", "text/plain", b"", None).await;
    }
    let token = segments[1].clone();
    let item_id = segments[2].clone();
    let rel_path = segments[3..].join("/");

    // token 校验
    if token != state.token {
        return respond(stream, 401, "Unauthorized", "text/plain", b"", None).await;
    }
    // itemId 白名单（guard 在闭包内释放，避免跨 await 持有非 Send 锁）
    let ok = state
        .db
        .lock()
        .map(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM library_items WHERE item_id = ?1",
                [item_id.as_str()],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false)
        })
        .unwrap_or(false);
    if !ok {
        return respond(stream, 404, "Not Found", "text/plain", b"", None).await;
    }

    // 路径规范化防穿越
    let base = state.wallpapers_dir.join(item_id);
    let Some(target) = normalize(&base, &rel_path) else {
        return respond(stream, 403, "Forbidden", "text/plain", b"", None).await;
    };
    if !target.starts_with(&base) {
        return respond(stream, 403, "Forbidden", "text/plain", b"", None).await;
    }

    // 目录 → index.html（web 壁纸站点根）
    let file = if target.is_dir() {
        let idx = target.join("index.html");
        if idx.is_file() {
            idx
        } else {
            return respond(stream, 404, "Not Found", "text/plain", b"", None).await;
        }
    } else {
        target
    };
    if !file.is_file() {
        return respond(stream, 404, "Not Found", "text/plain", b"", None).await;
    }

    // Range 支持
    let range = lines
        .find_map(|l| {
            let l = l.trim();
            l.to_ascii_lowercase()
                .strip_prefix("range:")
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_default();

    let data = tokio::fs::read(&file).await.map_err(|e| e.to_string())?;
    let total = data.len() as u64;
    let mime = mime_for(&file);

    if range.starts_with("bytes=") {
        let spec = &range[6..];
        let spec = spec.split(',').next().unwrap_or("").trim();
        if let Some((s, e)) = spec.split_once('-') {
            let start: u64 = s.parse().unwrap_or(0);
            let end: u64 = if e.is_empty() {
                total.saturating_sub(1)
            } else {
                e.parse().unwrap_or(total.saturating_sub(1)).min(total.saturating_sub(1))
            };
            if start <= end && start < total {
                let slice = &data[start as usize..=end as usize];
                let resp = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: {mime}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    slice.len()
                );
                let mut out = resp.into_bytes();
                out.extend_from_slice(slice.as_ref());
                return stream.write_all(&out).await.map_err(|e| e.to_string());
            }
        }
        return respond(
            stream,
            416,
            "Range Not Satisfiable",
            "text/plain",
            format!("Content-Range: bytes */{total}").as_bytes(),
            None,
        )
        .await;
    }

    respond(
        stream,
        200,
        "OK",
        &mime,
        &data,
        Some(&format!("Accept-Ranges: bytes\r\nContent-Length: {}", data.len())),
    )
    .await
}

async fn respond(
    stream: &mut tokio::net::TcpStream,
    code: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    extra_headers: Option<&str>,
) -> Result<(), String> {
    // 注意：base 头部已以 \r\n 结尾。当 extra_headers 为空时，若直接拼 \r\n 再拼
    // "Connection: close" 会多出一个空行，导致 "Connection: close" 被当成响应 body，
    // 浏览器会在页面顶部把这段文字渲染出来。这里统一保证 Connection: close 是最后一行头。
    let mut head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\n"
    );
    if let Some(extra) = extra_headers {
        head.push_str(extra);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(body);
    stream.write_all(&out).await.map_err(|e| e.to_string())
}

/// 规范化拼接路径（拒绝 .. 与绝对路径）
fn normalize(base: &Path, rel: &str) -> Option<PathBuf> {
    if rel.contains("..") || rel.starts_with('/') {
        return None;
    }
    Some(base.join(rel))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "pkg" => "application/octet-stream",
        "tex" => "application/octet-stream",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "svg" => "image/svg+xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[tauri::command]
pub fn content_server_status(app: AppHandle) -> serde_json::Value {
    let port = match app.try_state::<Arc<Mutex<u16>>>() {
        Some(p) => match p.lock() {
            Ok(g) => *g,
            Err(_) => 0,
        },
        None => 0,
    };
    let token = app
        .try_state::<ContentServerState>()
        .map(|s| s.token.clone())
        .unwrap_or_default();
    json!({ "port": port, "token": token, "base": format!("http://127.0.0.1:{port}") })
}
