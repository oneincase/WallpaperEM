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
    /// 系统音频捕获共享帧（二期：/audio-stream SSE 端点 + shim 引导标记）
    pub audio: Arc<crate::audio_capture::AudioShared>,
    /// 当前存活的 /audio-stream SSE 客户端数（壁纸页存活信号：新 shim 下每个
    /// web 壁纸页加载后必然持有 1 条连接；睡眠唤醒后若为 0 说明页面已僵死）
    pub sse_clients: Arc<std::sync::atomic::AtomicUsize>,
}

impl ContentServerState {
    /// 当前存活的 SSE 客户端数（壁纸引擎唤醒后检测页面僵死用）
    pub fn sse_client_count(&self) -> usize {
        self.sse_clients.load(std::sync::atomic::Ordering::Relaxed)
    }
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

    let audio = app
        .try_state::<crate::audio_capture::AudioCaptureState>()
        .map(|s| s.shared.clone())
        .ok_or("音频捕获状态未就绪（audio_capture::init 须先于 content_server::init）")?;

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
        audio,
        sse_clients: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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
        // 本地 vite 直连，绝不走系统代理（否则 dev 下渲染器页/媒体加载失败）
        let client = reqwest::Client::builder()
            .no_proxy()
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
        // 本地 vite 直连，绝不走系统代理
        let client = reqwest::Client::builder()
            .no_proxy()
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
        // 本地 vite 直连，绝不走系统代理
        let client = reqwest::Client::builder()
            .no_proxy()
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

    // 系统音频频谱推送（SSE，二期）：壁纸页内 shim 经 EventSource 订阅
    if let Some(tok) = path.strip_prefix("/audio-stream/") {
        if tok != state.token {
            return respond(stream, 401, "Unauthorized", "text/plain", b"", None).await;
        }
        return audio_stream_sse(stream, state).await;
    }

    // 目录属性随机文件（WE wallpaperRequestRandomFileForProperty 契约）：
    // /random-file/{token}/{item_id}/{相对目录} → {"file": "目录内随机文件相对路径"}
    // 仅限壁纸包内目录；目录不存在/为空返回 file=null（shim 据此不回调）
    if let Some(rest) = path.strip_prefix("/random-file/") {
        let segs: Vec<String> = rest.split('/').map(percent_decode).collect();
        if segs.len() < 3 {
            return respond(stream, 404, "Not Found", "text/plain", b"", None).await;
        }
        let (token, item_id) = (segs[0].clone(), segs[1].clone());
        let rel_dir = segs[2..].join("/");
        if token != state.token {
            return respond(stream, 401, "Unauthorized", "text/plain", b"", None).await;
        }
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
        let base = state.wallpapers_dir.join(&item_id);
        let Some(dir) = normalize(&base, &rel_dir) else {
            return respond(stream, 403, "Forbidden", "text/plain", b"", None).await;
        };
        if !dir.starts_with(&base) || !dir.is_dir() {
            return respond(stream, 404, "Not Found", "text/plain", b"", None).await;
        }
        let body = match random_file_in_dir(&dir, &rel_dir) {
            Some(file) => json!({ "file": file }),
            None => json!({ "file": null }),
        };
        return respond(
            stream,
            200,
            "OK",
            "application/json",
            body.to_string().as_bytes(),
            None,
        )
        .await;
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
    let base = state.wallpapers_dir.join(&item_id);
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
    let mime = mime_for(&file);

    // WE 网页壁纸兼容 shim：库内条目的 HTML 响应注入引导数据（属性/fps）+ 脚本，
    // 拼在 <head> 后先于壁纸自身脚本执行（属性监听、音频 API、rAF 节流均依赖此时机）
    let data = if mime.starts_with("text/html") {
        inject_we_shim(state, &item_id, data)
    } else if rel_path == "project.json" {
        // 场景壁纸的属性作用在 scene.json 的字段绑定上，渲染器是从 project.json 读属性表
        // 来解引用那些绑定的，故用户覆盖值必须合并进这份响应（网页壁纸走上面的 shim 下发）
        match state.db.lock() {
            Ok(conn) => crate::we_props::merge_overrides_into_project(&conn, &item_id, data),
            Err(_) => data,
        }
    } else {
        data
    };
    // 注意：改写响应体后长度会变，Range 必须按**改写后**的长度算，否则切片越界/错位
    let total = data.len() as u64;

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

/// SSE：以 ~30Hz 推送最新 64 段频谱（`data: [f0,...,f63]`）。
/// 捕获未运行时 503 —— 壁纸页内 EventSource 会自动重试，开关打开后自愈；
/// 客户端断开表现为写失败，结束本连接。
async fn audio_stream_sse(
    stream: &mut tokio::net::TcpStream,
    state: &ContentServerState,
) -> Result<(), String> {
    if !state.audio.is_running() {
        return respond(
            stream,
            503,
            "Service Unavailable",
            "text/plain",
            b"audio capture disabled",
            None,
        )
        .await;
    }
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\n\
              Content-Type: text/event-stream\r\n\
              Cache-Control: no-cache\r\n\
              Connection: close\r\n\
              Access-Control-Allow-Origin: *\r\n\r\n",
        )
        .await
        .map_err(|e| e.to_string())?;
    state.sse_clients.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let result = stream_loop_sse(stream, state).await;
    state
        .sse_clients
        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    result
}

/// SSE 推送主循环（连接计数由 audio_stream_sse 管理）
async fn stream_loop_sse(
    stream: &mut tokio::net::TcpStream,
    state: &ContentServerState,
) -> Result<(), String> {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(33)).await;
        if !state.audio.is_running() {
            return Ok(()); // 开关被关闭：正常结束，客户端稍后重连
        }
        let (seq, bands) = state.audio.snapshot();
        let data = format!(
            "data: [{}]\n\n",
            bands.iter().map(|v| format!("{v:.3}")).collect::<Vec<_>>().join(",")
        );
        // seq 仅用于调试判断新鲜度，此处无条件推送（静音时为 0 帧，保持客户端活动）
        let _ = seq;
        match stream.write_all(data.as_bytes()).await {
            Ok(_) => {
                let _ = stream.flush().await;
            }
            Err(e) => return Err(format!("audio sse client disconnected: {e}")),
        }
    }
}

/// WE 网页壁纸兼容层：把引导数据（project.json 属性 + fps）与 shim 脚本拼进 HTML。
/// 位置选 <head> 开标签之后（保证先于壁纸脚本执行）；无 <head> 则前置。
fn inject_we_shim(state: &ContentServerState, item_id: &str, html: Vec<u8>) -> Vec<u8> {
    let mut boot = crate::we_props::boot_json(&state.db, &state.wallpapers_dir, item_id);
    // 系统音频：SSE 端点地址（壁纸页与内容服务器同源，EventSource 直接订阅）
    boot["token"] = serde_json::json!(state.token);
    boot["systemAudio"] = serde_json::json!(state.audio.is_running());
    // 属性文本可能含 script 结束标签：JSON 内把 "<" 转义为 \u003c（合法 JSON 转义，
    // 解析回原文本），确保注入文本中不出现标签结束序列
    let boot_str = serde_json::to_string(&boot)
        .unwrap_or_else(|_| "{}".into())
        .replace("</", "\\u003c/");
    let prelude = format!(
        "<script>window.__WE_BOOT={boot_str};</script><script>{}</script>",
        crate::we_shim::SRC
    );
    let text = String::from_utf8_lossy(&html);
    let lower = text.to_ascii_lowercase();
    let insert_at = find_head_open(&lower)
        .and_then(|pos| lower[pos..].find('>').map(|off| pos + off + 1))
        .unwrap_or(0);
    let mut out = String::with_capacity(text.len() + prelude.len());
    out.push_str(&text[..insert_at]);
    out.push_str(&prelude);
    out.push_str(&text[insert_at..]);
    out.into_bytes()
}

/// 找真正的 `<head` 开标签（排除 `<header` 等以 head 开头的标签名）
fn find_head_open(lower: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = lower[from..].find("<head") {
        let pos = from + rel;
        let after = &lower[pos + 5..];
        let ok = after.starts_with('>')
            || after.starts_with('/')
            || after.starts_with(' ')
            || after.starts_with('\t')
            || after.starts_with('\n')
            || after.starts_with('\r');
        if ok {
            return Some(pos);
        }
        from = pos + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 离线体检：对真实壁纸库（web 类型）里所有 HTML 文件跑生产注入，
    /// 产物写到 WPEM_DUMP_DIR（相对镜像路径，供无头浏览器渲染验证服务使用），
    /// 并断言注入点/次数不变式。设 WPEM_DUMP_DIR 才生效（CI/常规测试无库可跳过）。
    #[test]
    fn dump_injected_html_for_local_library() {
        let Ok(dump_root) = std::env::var("WPEM_DUMP_DIR") else {
            return;
        };
        let wallpapers_dir = std::path::PathBuf::from(
            std::env::var("WPEM_WALLPAPERS_DIR").unwrap_or_else(|_| {
                dirs_shim().join("wallpapers").to_string_lossy().into_owned()
            }),
        );
        // 直接只读打开真实库 DB 副本：取 web 条目清单 + 用户属性覆盖 + fps，
        // 与生产 boot_json 完全同源（effective_props 只依赖 settings 表）
        let db_path = std::env::var("WPEM_DB_COPY").unwrap_or_default();
        let conn = if db_path.is_empty() {
            Connection::open_in_memory().unwrap()
        } else {
            Connection::open_with_flags(
                &db_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap()
        };
        let db = Arc::new(Mutex::new(conn));
        let state = ContentServerState {
            port: 0,
            token: "dump".into(),
            db: db.clone(),
            wallpapers_dir: wallpapers_dir.clone(),
            renderer_dir: Default::default(),
            audio: std::sync::Arc::new(crate::audio_capture::AudioShared::new()),
            sse_clients: Default::default(),
        };

        let items: Vec<String> = {
            let conn = db.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT item_id FROM library_items WHERE lower(type)='web'")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .filter_map(Result::ok)
                .collect()
        };
        let mut dumped = 0usize;
        let mut problems: Vec<String> = Vec::new();
        println!("library web items: {}", items.len());
        for item in &items {
            let dir = wallpapers_dir.join(item);
            // 入口优先级与 wallpaper::resolve_item_config 的 web 分支一致
            let entry = crate::wallpaper::project_json_entry(&dir)
                .or_else(|| {
                    (dir.join("web/index.html").is_file()).then(|| "web/index.html".to_string())
                })
                .or_else(|| (dir.join("index.html").is_file()).then(|| "index.html".to_string()))
                .or_else(|| crate::wallpaper::find_first_html(&dir));
            if entry.is_none() {
                problems.push(format!("{item}: 无任何 HTML 入口（无法应用）"));
                continue;
            }
            std::fs::create_dir_all(std::path::Path::new(&dump_root).join(item)).unwrap();
            // 库内所有 HTML 响应在生产都会被注入（不止入口页），逐个镜像
            let mut htmls: Vec<std::path::PathBuf> = Vec::new();
            collect_htmls(&dir, &mut htmls);
            for path in htmls {
                let Ok(raw) = std::fs::read(&path) else { continue };
                let rel = path.strip_prefix(&dir).unwrap().to_string_lossy().into_owned();
                let out = inject_we_shim(&state, item, raw);
                let text = String::from_utf8_lossy(&out).into_owned();
                let lower = text.to_ascii_lowercase();
                // 不变式 1：prelude 恰好一次
                let boot_n = lower.matches("window.__we_boot={").count();
                if boot_n != 1 {
                    problems.push(format!("{item}/{rel}: __WE_BOOT 出现 {boot_n} 次"));
                }
                // 不变式 2：有 <head> 时注入点必须在 <head 开标签之内（'</head' 之前）
                if lower.contains("<head") {
                    let head_ins = find_head_open(&lower).unwrap();
                    assert!(
                        lower[head_ins..].contains("__we_boot"),
                        "{item}/{rel}: 注入点不在 <head> 内"
                    );
                    let head_end = lower.find("</head").unwrap();
                    assert!(
                        head_ins < head_end,
                        "{item}/{rel}: 注入点落在 </head> 之后"
                    );
                }
                let dest = std::path::Path::new(&dump_root).join(item).join(&rel);
                std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                std::fs::write(&dest, out).unwrap();
                dumped += 1;
            }
        }
        std::fs::write(
            std::path::Path::new(&dump_root).join("_problems.json"),
            serde_json::to_string_pretty(&problems).unwrap(),
        )
        .unwrap();
        println!("dumped {dumped} injected html files, {} problems", problems.len());
        for p in &problems {
            println!("  ⚠ {p}");
        }
    }

    fn collect_htmls(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_htmls(&p, out);
            } else if p.extension().and_then(|x| x.to_str()).map(|x| {
                x.eq_ignore_ascii_case("html") || x.eq_ignore_ascii_case("htm")
            }) == Some(true)
            {
                out.push(p);
            }
        }
    }

    /// 无 dirs crate 时的 home 目录兜底（仅测试用）
    fn dirs_shim() -> std::path::PathBuf {
        std::env::var("HOME").map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
    }

    /// 随机文件端点的目录挑选逻辑：只挑普通文件、路径相对壁纸根、空目录 None
    #[test]
    fn random_file_in_dir_picks_files_relative_to_root() {
        let root = std::env::temp_dir().join("wpem-random-file-test");
        let _ = std::fs::remove_dir_all(&root);
        let album = root.join("directories/album");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("a.png"), b"x").unwrap();
        std::fs::write(album.join("b.jpg"), b"y").unwrap();
        std::fs::create_dir_all(album.join("nested")).unwrap();
        // 多次取样都落在普通文件集合内（嵌套目录不参与；路径相对壁纸根）
        for _ in 0..8 {
            let pick = random_file_in_dir(&album, "directories/album").unwrap();
            assert!(
                pick == "directories/album/a.png" || pick == "directories/album/b.jpg",
                "unexpected pick: {pick}"
            );
        }
        // 空目录 → None
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(random_file_in_dir(&empty, "empty"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 找到 <head> 后插入点应在开标签 `>` 之后（即 prelude 位于 </head> 之前、
    /// 壁纸自身脚本之前）
    #[test]
    fn find_head_open_variants() {
        // 注意：入参约定为已 to_ascii_lowercase 的小写文本（与生产调用一致）
        assert_eq!(find_head_open("<html><head><title>x"), Some(6));
        assert_eq!(find_head_open("<html><head lang=\"zh\">"), Some(6));
        assert_eq!(find_head_open("<html><head\n class=\"a\">"), Some(6));
        // <header> 不是 <head>
        assert_eq!(find_head_open("<body><header class=\"h\">x"), None);
        // <header 在前、真 <head> 在后（"<header>" 8 字符 + "</header>" 9 字符 → 偏移 17）
        assert_eq!(find_head_open("<header></header><head>"), Some(17));
        // 无 head：前置到文档开头
        assert_eq!(find_head_open("<html><body>"), None);
    }

    #[test]
    fn inject_splices_after_head_and_escapes_closing_tag() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL)", [])
            .unwrap();
        let dir = std::env::temp_dir().join("wpem-inject-test-item");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.json"),
            r#"{"type":"web","file":"index.html","general":{"properties":{"t":{"type":"text","value":"a</script>b"}}}}"#,
        )
        .unwrap();
        let state = ContentServerState {
            port: 0,
            token: "t".into(),
            db: Arc::new(Mutex::new(conn)),
            wallpapers_dir: std::env::temp_dir(),
            renderer_dir: Default::default(),
            audio: std::sync::Arc::new(crate::audio_capture::AudioShared::new()),
            sse_clients: Default::default(),
        };
        let html = b"<!DOCTYPE html><html><head><meta charset=utf-8></head><body></body></html>".to_vec();
        let out = inject_we_shim(&state, "wpem-inject-test-item", html);
        let s = String::from_utf8(out).unwrap();
        // prelude 拼在 <head …> 之后、</head> 之前
        let (before, after) = s.split_once("</head>").unwrap();
        assert!(before.contains("__WE_BOOT"), "引导数据应在 </head> 之前");
        assert!(before.contains("__weSetFps"), "shim 控制接口应在 </head> 之前");
        assert!(after.contains("<body>"));
        // 属性值里的标签结束序列被 JSON unicode 转义（解析回原文本，且不会提前闭合标签）
        assert!(s.contains(r"a\u003c/script>b"), "值中的结束标签已转义");
        assert!(!s.contains("a</script>b"), "原文中的结束标签不应原样出现");
        // 引导赋值只出现一次（shim 内的读取引用不算；一次注入，不会叠加）
        assert_eq!(s.matches("window.__WE_BOOT={").count(), 1);
    }
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

/// 目录内（非递归）随机挑一个普通文件，返回「相对壁纸根」的路径（URL 时按段再编码）。
/// 目录为空或没有普通文件返回 None。
fn random_file_in_dir(dir: &Path, rel_prefix: &str) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    let files: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // 提前拒绝 HTML？WE 目录属性会返回任意文件（幻灯片场景以图片为主），
            // 不做类型过滤，交由壁纸自身处理
            Some(if rel_prefix.is_empty() {
                name
            } else {
                format!("{}/{}", rel_prefix.trim_end_matches('/'), name)
            })
        })
        .collect();
    if files.is_empty() {
        return None;
    }
    // 轻量随机：时间熵 + 洗牌取首（避免引入 rand 依赖）
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize ^ (d.as_secs() as usize))
        .unwrap_or(0);
    let idx = now % files.len();
    Some(files.into_iter().nth(idx).unwrap())
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
