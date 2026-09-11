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
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

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
    /// 系统「正在播放」共享快照（/now-playing SSE 端点）
    pub media: Arc<crate::now_playing::MediaShared>,
    /// 当前存活的 /audio-stream SSE 客户端数（壁纸页存活信号：新 shim 下每个
    /// web 壁纸页加载后必然持有 1 条连接；睡眠唤醒后若为 0 说明页面已僵死）
    pub sse_clients: Arc<std::sync::atomic::AtomicUsize>,
    /// 渲染器「ready」信号时间戳（epoch millis）：壁纸挂载成功后经 /diag 回流。
    /// system_wallpaper 的场景/网页截图等它，避免截到未渲染完成的黑屏
    pub wallpaper_ready_ms: Arc<std::sync::atomic::AtomicU64>,
    /// 渲染器诊断快照：截图超时时用来说明「到底卡在哪一步」，
    /// 也用来判断「同一张壁纸是不是已经挂好了」（免掉重复挂载的整轮解析）
    pub renderer_diag: Arc<Mutex<RendererDiag>>,
}

/// 渲染器最近一次诊断的汇总快照（`/diag` 每次上报都会更新）
#[derive(Default)]
pub struct RendererDiag {
    /// 最近一条诊断原文，如 `[scene 123] mount start`
    pub last: String,
    /// 最近一条 `failed: …` 的原因（空串表示本次运行还没报过失败）
    pub fail_reason: String,
    /// 最近一次 `failed:` 的时间戳（epoch millis），0 表示没失败过
    pub fail_ms: u64,
    /// 最近一次 `ready` 对应的本地库条目 id（解析自诊断文本的 src 段）
    pub ready_item: String,
}

impl ContentServerState {
    /// 当前存活的 SSE 客户端数（壁纸引擎唤醒后检测页面僵死用）
    pub fn sse_client_count(&self) -> usize {
        self.sse_clients.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// 从渲染器诊断文本里抽本地库条目 id。
///
/// 诊断前缀是 `[<type> <src>] <msg>`（见 renderer/src/main.ts 的 reportDiag）：
///   - scene：`src` 就是 itemId                       → `[scene 1948961570] ready`
///   - web  ：`src` 是 `/web/<token>/<itemId>/…` 的 URL → 取 `<itemId>` 段
/// 解析失败返回 None（比如原本就没有 src 的 canvas 类型）。
fn diag_item_id(msg: &str) -> Option<String> {
    let inner = msg.strip_prefix('[')?.split_once(']')?.0;
    let (_ty, src) = inner.split_once(' ')?;
    if let Some(rest) = src.split("/web/").nth(1).or_else(|| src.split("/media/").nth(1)) {
        // rest = "<token>/<itemId>/…"
        let item = rest.split('/').nth(1).unwrap_or_default();
        return (!item.is_empty()).then(|| item.to_string());
    }
    let src = src.trim();
    (!src.is_empty() && !src.contains(' ') && !src.contains('/')).then(|| src.to_string())
}

/// 渲染器最近一次就绪的条目 id（没有 ready 过则 None）。
/// MCP 截图用它判断「这张壁纸已经挂在屏上了」，重复截图时不必再挂一次。
pub fn ready_item(app: &tauri::AppHandle) -> Option<String> {
    let state = app.try_state::<ContentServerState>()?;
    let d = state.renderer_diag.lock().ok()?;
    let ms = state
        .wallpaper_ready_ms
        .load(std::sync::atomic::Ordering::Relaxed);
    (ms > 0 && !d.ready_item.is_empty()).then(|| d.ready_item.clone())
}

/// 记一条渲染器诊断：ready 记时间戳（供截图等待）+ 条目 id（供重复截图免重挂），
/// `failed: …` 记原因（供超时报错引用），其余只留最近一条原文。
fn record_renderer_diag(state: &ContentServerState, msg: &str) {
    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    };
    if msg.ends_with("] ready") {
        state
            .wallpaper_ready_ms
            .store(now_ms(), std::sync::atomic::Ordering::Relaxed);
    }
    if let Ok(mut d) = state.renderer_diag.lock() {
        d.last = msg.to_string();
        if msg.ends_with("] ready") {
            if let Some(id) = diag_item_id(msg) {
                d.ready_item = id;
            }
        }
        if let Some((_, reason)) = msg.split_once("] failed: ") {
            d.fail_reason = reason.to_string();
            d.fail_ms = now_ms();
        }
    }
}

/// 给「等待渲染器就绪超时」类错误配一句人能看懂的原因（渲染器自报失败优先，
/// 否则给最近一条诊断 —— 常见是停在 `mount start`，说明还在解析大 scene.pkg）
// 目前只有 macOS 的壁纸截图自检（system_wallpaper::capture_wallpaper_png）会用到
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn renderer_diag_hint(app: &tauri::AppHandle) -> String {
    let Some(state) = app.try_state::<ContentServerState>() else {
        return String::new();
    };
    let Ok(d) = state.renderer_diag.lock() else {
        return String::new();
    };
    if !d.fail_reason.is_empty() {
        format!("渲染器自报失败：{}", d.fail_reason)
    } else if !d.last.is_empty() {
        format!("渲染器最近诊断：{}", d.last)
    } else {
        "渲染器未上报任何诊断（壁纸页可能根本没加载）".into()
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
        media: crate::now_playing::shared(),
        sse_clients: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        wallpaper_ready_ms: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        renderer_diag: Default::default(),
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
        tracing::info!(
            "content server listening on 127.0.0.1:{port} (token={})",
            state.token
        );

        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let state = state.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = handle_conn(&mut stream, &state).await {
                    // 视频/WebCodecs 拉流时中止 Range 请求是常态（缓冲够了就取消），
                    // Broken pipe / Connection reset 不算异常，不刷日志
                    if e.contains("Broken pipe") || e.contains("Connection reset") {
                        tracing::trace!("content server conn closed: {e}");
                    } else {
                        tracing::debug!("content server conn error: {e}");
                    }
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
    query: &str,
    state: &ContentServerState,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        // dev：代理到 vite dev server（与 tauri.conf devUrl 一致）
        let vite_path = path.trim_start_matches('/');
        // query 必须原样带上：vite 预打包依赖靠 `?v=<hash>` 区分版本，
        // 丢掉它会拿到 404 或过期产物
        let upstream = if query.is_empty() {
            format!("http://localhost:1420/{vite_path}")
        } else {
            format!("http://localhost:1420/{vite_path}?{query}")
        };
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
        let res = state
            .renderer_dir
            .join(path.trim_start_matches("/renderer/"));
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
        return proxy_renderer(stream, path, query, state).await;
    }

    // dev：渲染器改用 npm 依赖（webwallgl）后，vite 把裸模块重写成
    // `/node_modules/.vite/deps/xxx.js?v=<hash>` 绝对路径。这类请求不在
    // /renderer 前缀下，必须单独放行，否则渲染页一进来就 404、整个模块不执行
    // （症状：壁纸窗口全黑，且连 /diag 诊断都发不出来）。
    // prod 由 vite 打包进 /assets，不会出现该路径。
    #[cfg(debug_assertions)]
    if path.starts_with("/node_modules") || path.starts_with("/@vite") || path.starts_with("/@id") {
        return proxy_renderer(stream, path, query, state).await;
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
        record_renderer_diag(&state, &msg);
        tracing::warn!("[renderer diag] {msg}");
        return respond(stream, 200, "OK", "text/plain", b"ok", None).await;
    }

    // 壁纸生效属性（场景/媒体壁纸挂载前拉取一次，作为 mount() 的 properties 选项）。
    //
    // 网页壁纸走 HTML 改写时的 __weSeedProps，已有注入路径；场景/媒体壁纸没有
    // 入口 HTML 可改写，只能由渲染器在挂载前显式拉一次。返回的就是 effective_props
    // （project.json 默认 + 用户覆盖 + 全局语言兜底），与网页壁纸同源，避免两边
    // 对"某属性当前是什么值"给出不同答案。
    //   /props/{token}/{item_id}  →  {"name":{"value":...}, ...}
    if let Some(rest) = path.strip_prefix("/props/") {
        let segs: Vec<&str> = rest.split('/').collect();
        if segs.len() < 2 || segs[0] != state.token {
            return respond(stream, 401, "Unauthorized", "text/plain", b"", None).await;
        }
        let item_id = percent_decode(segs[1]);
        let props = {
            match state.db.lock() {
                Ok(conn) => {
                    crate::we_props::effective_props(&conn, &state.wallpapers_dir, &item_id)
                }
                Err(_) => serde_json::Map::new(),
            }
        };
        let body = serde_json::to_vec(&props).unwrap_or_default();
        return respond(stream, 200, "OK", "application/json", &body, None).await;
    }

    // 系统音频频谱推送（SSE，二期）：壁纸页内 shim 经 EventSource 订阅
    if let Some(tok) = path.strip_prefix("/audio-stream/") {
        if tok != state.token {
            return respond(stream, 401, "Unauthorized", "text/plain", b"", None).await;
        }
        return audio_stream_sse(stream, state).await;
    }

    // 系统「正在播放」推送（SSE）：歌名/艺人/专辑/进度/封面，喂库的 setMedia
    if let Some(tok) = path.strip_prefix("/now-playing/") {
        if tok != state.token {
            return respond(stream, 401, "Unauthorized", "text/plain", b"", None).await;
        }
        return now_playing_sse(stream, state).await;
    }

    // 媒体反向控制：壁纸里的播放/暂停、上下一曲按钮转发给真实播放器。
    // /media-command/{token}/{play|pause|playPause|next|previous}
    // 走 HTTP 而非 Tauri 命令：壁纸页是 iframe 里的普通网页，没有 IPC 通道。
    // 用 GET 而非 POST：这个服务器整体只放行 GET/OPTIONS，而端点只监听本机、
    // 带 token 鉴权，没必要为一个按钮给全局开 POST
    if let Some(rest) = path.strip_prefix("/media-command/") {
        let mut segs = rest.splitn(2, '/');
        let tok = segs.next().unwrap_or("");
        let cmd = segs.next().unwrap_or("");
        if tok != state.token {
            return respond(stream, 401, "Unauthorized", "text/plain", b"", None).await;
        }
        let Some(cmd) = crate::now_playing::MediaCommand::parse(cmd) else {
            return respond(
                stream,
                400,
                "Bad Request",
                "text/plain",
                b"unknown command",
                None,
            )
            .await;
        };
        let body: &[u8] = match crate::now_playing::send_command(cmd) {
            Ok(()) => b"ok",
            Err(e) => {
                tracing::warn!("media command failed: {e}");
                b"failed"
            }
        };
        return respond(stream, 200, "OK", "text/plain", body, None).await;
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

    let mime = mime_for(&file);

    // 需要改写响应体的小文件（HTML 注入 shim / project.json 合并属性覆盖）才整读；
    // 其余文件（视频/音频/图片，尤其 4K 视频）走流式服务：**绝不整文件进内存**。
    // 旧实现对每个请求都整读再切片，而 WebKit 播放期会发大量小段 Range 请求 ——
    // 每次请求都付出「读整个视频」的磁盘与内存代价；循环交接处备用元素的首批
    // 取数因此被拖慢 ~1s，表现为壁纸每圈卡一下。
    if !mime.starts_with("text/html") && rel_path != "project.json" {
        return serve_file_stream(stream, &file, &mime, &range).await;
    }

    let data = tokio::fs::read(&file).await.map_err(|e| e.to_string())?;
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
        if let Some((start, end)) = parse_range(&range, total) {
            {
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
        Some(&format!(
            "Accept-Ranges: bytes\r\nContent-Length: {}",
            data.len()
        )),
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
    state
        .sse_clients
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            bands
                .iter()
                .map(|v| format!("{v:.3}"))
                .collect::<Vec<_>>()
                .join(",")
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

/// SSE：系统「正在播放」快照。与音频端点不同，这里**只在快照变化时**推送
/// —— 封面 base64 有上百 KB，按帧推会把连接打满。
///
/// 进度（position）例外：播放中它每秒都在变，但 now_playing 侧只在收到系统通知
/// 时更新，所以这里按固定间隔重推一次让壁纸的进度条能走动。壁纸自己也会用
/// position + 本地时钟外推，所以这个间隔不必很密。
async fn now_playing_sse(
    stream: &mut tokio::net::TcpStream,
    state: &ContentServerState,
) -> Result<(), String> {
    // adapter 起不来（perl 缺失 / Apple 封了这条路）时 503：EventSource 会自动
    // 重试，若后续恢复即自愈。壁纸侧因此不会装上一个永远空的媒体源
    if !state.media.is_available() {
        return respond(
            stream,
            503,
            "Service Unavailable",
            "text/plain",
            b"now playing unavailable",
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

    let mut last_seq = u64::MAX; // 保证首轮必发一次当前状态（含「无媒体」）
    let mut ticks = 0u32;
    loop {
        let (seq, snap) = state.media.snapshot();
        // 内容变了就发；否则每 ~2s 补一次让播放进度前进（约 8 * 250ms）
        let heartbeat = snap.state == 1 && ticks % 8 == 0;
        if seq != last_seq || heartbeat {
            last_seq = seq;
            let json = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
            let data = format!("data: {json}\n\n");
            match stream.write_all(data.as_bytes()).await {
                Ok(_) => {
                    let _ = stream.flush().await;
                }
                Err(e) => return Err(format!("now-playing sse client disconnected: {e}")),
            }
        }
        ticks = ticks.wrapping_add(1);
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

///
/// 为什么由 host 注入而不是交给库：库的 mountWeb 对**同源**入口只做 attachIframe
/// （保留原 URL，壁纸的相对资源才能正常加载），注入交给 host；只有跨源时它才
/// 走 fetch + rewriteHtml + blob URL 那条路。我们的壁纸都经内容服务器同源提供，
/// 所以走的是前者 —— shim 必须由这里注入，且必须是**库自带的那一份**
/// （src/we_shim.js 从 bundle 提取），否则库的音频/媒体泵调 __wePushAudio
/// 等接口时会全部落空。
///
/// 种子脚本用库的协议（__weSetFps / __weSetVolume / __weSeedProps），
/// 与库 buildSeedScript 产出的形式一致。位置选 <head> 开标签之后，
/// 保证先于壁纸自身脚本执行。
fn inject_we_shim(state: &ContentServerState, item_id: &str, html: Vec<u8>) -> Vec<u8> {
    let boot = crate::we_props::boot_json(&state.db, &state.wallpapers_dir, item_id);
    let fps = boot.get("fps").and_then(|v| v.as_i64()).unwrap_or(60);
    let props = boot
        .get("props")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    // 属性文本可能含 script 结束标签：JSON 内把 "<" 转义为 \u003c（合法 JSON 转义，
    // 解析回原文本），确保注入文本中不出现标签结束序列
    let props_str = serde_json::to_string(&props)
        .unwrap_or_else(|_| "{}".into())
        .replace("</", "\\u003c/");

    let mut seed = format!("window.__weSetFps({fps});");
    if props.as_object().map(|m| !m.is_empty()).unwrap_or(false) {
        seed.push_str(&format!("window.__weSeedProps({props_str});"));
    }
    // 目录属性的文件清单：库的 shim 从父页预推的清单里挑随机文件
    // （官方 CEF 直接读文件系统，浏览器里做不到）。不推的话
    // wallpaperRequestRandomFileForProperty 恒回空串，幻灯片壁纸的图片永远不换。
    let dir_files = match state.db.lock() {
        Ok(conn) => crate::we_props::directory_files(&conn, &state.wallpapers_dir, item_id),
        Err(_) => Default::default(),
    };
    for (prop, files) in &dir_files {
        let payload = serde_json::to_string(files)
            .unwrap_or_else(|_| "[]".into())
            .replace("</", "\\u003c/");
        let prop_str = serde_json::to_string(prop)
            .unwrap_or_else(|_| "\"\"".into())
            .replace("</", "\\u003c/");
        seed.push_str(&format!(
            "window.__wePushDirectoryFiles&&window.__wePushDirectoryFiles({prop_str},{payload});"
        ));
    }
    // 系统音频经父页的音频泵推入（__wePushAudio），shim 不再自行订阅 SSE ——
    // 一份数据两处订阅会让 iframe 里外各建一条连接，且 shim 侧无法参与库的
    // 「外部帧优先」逻辑。token 仍下发给渲染器页（URL 的 audioToken）。
    let prelude = format!(
        "<script>{}</script><script>{seed}</script>",
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
        let wallpapers_dir =
            std::path::PathBuf::from(std::env::var("WPEM_WALLPAPERS_DIR").unwrap_or_else(|_| {
                dirs_shim()
                    .join("wallpapers")
                    .to_string_lossy()
                    .into_owned()
            }));
        // 直接只读打开真实库 DB 副本：取 web 条目清单 + 用户属性覆盖 + fps，
        // 与生产 boot_json 完全同源（effective_props 只依赖 settings 表）
        let db_path = std::env::var("WPEM_DB_COPY").unwrap_or_default();
        let conn = if db_path.is_empty() {
            Connection::open_in_memory().unwrap()
        } else {
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
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
            media: crate::now_playing::shared(),
            sse_clients: Default::default(),
            wallpaper_ready_ms: Default::default(),
            renderer_diag: Default::default(),
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
                let Ok(raw) = std::fs::read(&path) else {
                    continue;
                };
                let rel = path
                    .strip_prefix(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let out = inject_we_shim(&state, item, raw);
                let text = String::from_utf8_lossy(&out).into_owned();
                let lower = text.to_ascii_lowercase();
                // 不变式 1：种子脚本恰好一次
                let boot_n = lower.matches("window.__weseedprops(").count()
                    + lower.matches("window.__wesetfps(").count();
                if boot_n == 0 {
                    problems.push(format!("{item}/{rel}: 未注入种子脚本"));
                }
                // 不变式 2：有 <head> 时注入点必须在 <head 开标签之内（'</head' 之前）
                if lower.contains("<head") {
                    let head_ins = find_head_open(&lower).unwrap();
                    assert!(
                        lower[head_ins..].contains("__wesetfps"),
                        "{item}/{rel}: 注入点不在 <head> 内"
                    );
                    let head_end = lower.find("</head").unwrap();
                    assert!(head_ins < head_end, "{item}/{rel}: 注入点落在 </head> 之后");
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
        println!(
            "dumped {dumped} injected html files, {} problems",
            problems.len()
        );
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
            } else if p
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("html") || x.eq_ignore_ascii_case("htm"))
                == Some(true)
            {
                out.push(p);
            }
        }
    }

    /// 无 dirs crate 时的 home 目录兜底（仅测试用）
    fn dirs_shim() -> std::path::PathBuf {
        std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
    }

    /// 构造一个只读用途的 ContentServerState（诊断快照测试用）
    fn diag_test_state() -> ContentServerState {
        ContentServerState {
            port: 0,
            token: "t".into(),
            db: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            wallpapers_dir: std::env::temp_dir(),
            renderer_dir: Default::default(),
            audio: std::sync::Arc::new(crate::audio_capture::AudioShared::new()),
            media: crate::now_playing::shared(),
            sse_clients: Default::default(),
            wallpaper_ready_ms: Default::default(),
            renderer_diag: Default::default(),
        }
    }

    /// 诊断前缀 `[<type> <src>] <msg>` 里抽条目 id：
    /// scene 的 src 直接是 itemId；web 的 src 是 /web/<token>/<itemId>/… 的 URL
    #[test]
    fn diag_item_id_parses_scene_and_web_sources() {
        assert_eq!(
            diag_item_id("[scene 1948961570] ready").as_deref(),
            Some("1948961570")
        );
        assert_eq!(
            diag_item_id("[web http://127.0.0.1:8080/web/tok123/abc456/index.html] ready").as_deref(),
            Some("abc456")
        );
        assert_eq!(
            diag_item_id("[media http://127.0.0.1:8080/media/tok123/abc456/a.mp4] ready")
                .as_deref(),
            Some("abc456")
        );
        // canvas 没有 src；带路径的非 web 形态也不硬猜，一律 None
        assert_eq!(diag_item_id("[canvas] ready"), None);
        assert_eq!(diag_item_id("没有前缀"), None);
    }

    /// ready → 记时间戳 + 条目 id；failed: → 记原因（截图超时报错要引用它）
    #[test]
    fn record_renderer_diag_tracks_ready_and_failure() {
        let state = diag_test_state();
        assert_eq!(
            state.wallpaper_ready_ms.load(std::sync::atomic::Ordering::Relaxed),
            0
        );

        record_renderer_diag(&state, "[scene 111] mount start");
        assert_eq!(
            state.wallpaper_ready_ms.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "mount start 不是 ready，不该动时间戳"
        );

        record_renderer_diag(&state, "[scene 111] ready");
        assert!(
            state.wallpaper_ready_ms.load(std::sync::atomic::Ordering::Relaxed) > 0,
            "ready 必须记时间戳"
        );
        {
            let d = state.renderer_diag.lock().unwrap();
            assert_eq!(d.ready_item, "111");
            assert!(d.last.ends_with("ready"));
            assert!(d.fail_reason.is_empty());
        }

        record_renderer_diag(&state, "[scene 111] failed: scene.pkg 加载失败");
        let d = state.renderer_diag.lock().unwrap();
        assert_eq!(d.fail_reason, "scene.pkg 加载失败");
        assert!(d.fail_ms > 0);
        // 失败不该把「已就绪的条目」清掉（重试同一张时仍可跳过重复挂载）
        assert_eq!(d.ready_item, "111");
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

    /// 注入的 shim 必须与 webwallgl bundle 里那份逐字节一致。
    ///
    /// 库的 mountWeb 对同源 iframe 把 shim 注入交给 host，之后它的音频/媒体泵
    /// 直接调 __wePushAudio / __wePushMedia 等接口。两边一旦漂移（升级库但忘了跑
    /// scripts/sync-we-shim.cjs），壁纸仍能显示，只是音频可视化永远不动 ——
    /// 这种「看起来没坏」的故障最难查，用测试把它变成明确失败。
    ///
    /// node_modules 不存在时跳过（CI 只跑 cargo 的场景）。
    #[test]
    fn shim_matches_library_bundle() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let bundle = repo.join("node_modules/webwallgl/webwallgl.mjs");
        if !bundle.is_file() {
            eprintln!("跳过：{} 不存在（未装依赖）", bundle.display());
            return;
        }
        let src = std::fs::read_to_string(&bundle).unwrap();
        let key = "const shimSource = ";
        let at = src
            .find(key)
            .expect("bundle 里没有 shimSource —— 库内部结构变了");
        let start = at + key.len();
        let quote = src[start..].chars().next().unwrap();
        assert!(quote == '\'' || quote == '"', "shimSource 不是字符串字面量");

        // 逐字符扫到未转义的收尾引号
        let bytes: Vec<char> = src[start + 1..].chars().collect();
        let mut raw = String::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '\\' && i + 1 < bytes.len() {
                raw.push(bytes[i]);
                raw.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if bytes[i] == quote {
                break;
            }
            raw.push(bytes[i]);
            i += 1;
        }

        // 还原转义（与 scripts/sync-we-shim.cjs 同一套规则）
        let mut expected = String::with_capacity(raw.len());
        let chars: Vec<char> = raw.chars().collect();
        let mut j = 0;
        while j < chars.len() {
            if chars[j] == '\\' && j + 1 < chars.len() {
                match chars[j + 1] {
                    'n' => expected.push('\n'),
                    't' => expected.push('\t'),
                    'r' => expected.push('\r'),
                    '\\' => expected.push('\\'),
                    '\'' => expected.push('\''),
                    '"' => expected.push('"'),
                    '0' => expected.push('\0'),
                    // 未知转义序列原样保留两个字符
                    other => {
                        expected.push('\\');
                        expected.push(other);
                    }
                }
                j += 2;
            } else {
                expected.push(chars[j]);
                j += 1;
            }
        }

        assert_eq!(
            crate::we_shim::SRC,
            expected,
            "we_shim.js 与 webwallgl bundle 不一致 —— 请运行 `node scripts/sync-we-shim.cjs`"
        );
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

    /// 会被脚本"按类型消费"的资源必须有正确 Content-Type。
    ///
    /// 回归防护：`XMLHttpRequest.responseXML` 只在 Content-Type 属 XML 类型时才
    /// 解析，标成 application/octet-stream 会返回 null。3406740580（pano2vr
    /// 全景）用 responseXML 读 pano.xml，漏了 xml 映射就整张壁纸空屏，且
    /// HTTP 200、无控制台报错 —— 极难从现象反推，故用测试锁住。
    #[test]
    fn mime_for_covers_script_consumed_types() {
        // XML 家族（responseXML / DOMParser 依赖）
        assert!(
            mime_for(Path::new("pano.xml")).contains("xml"),
            "pano.xml 必须报 XML 类型，否则 responseXML 返回 null（3406740580 空屏）"
        );
        assert_eq!(mime_for(Path::new("a.svg")), "image/svg+xml");
        // 文本
        assert!(mime_for(Path::new("cfg.txt")).starts_with("text/plain"));
        assert!(mime_for(Path::new("data.csv")).starts_with("text/plain"));
        assert_eq!(mime_for(Path::new("sub.vtt")), "text/vtt");
        // 常见媒体/字体
        assert_eq!(mime_for(Path::new("a.m4a")), "audio/mp4");
        assert_eq!(mime_for(Path::new("a.flac")), "audio/flac");
        assert_eq!(mime_for(Path::new("f.ttf")), "font/ttf");
        assert_eq!(mime_for(Path::new("f.otf")), "font/otf");
        assert_eq!(mime_for(Path::new("i.avif")), "image/avif");
        // 大小写不敏感
        assert!(mime_for(Path::new("PANO.XML")).contains("xml"));
        // 私有二进制仍应是 octet-stream（不要为了"更准"给它们编类型）
        assert_eq!(mime_for(Path::new("scene.pkg")), "application/octet-stream");
        assert_eq!(mime_for(Path::new("x.tex")), "application/octet-stream");
        // 未知扩展名兜底
        assert_eq!(
            mime_for(Path::new("x.unknownext")),
            "application/octet-stream"
        );
    }

    #[test]
    fn inject_splices_after_head_and_escapes_closing_tag() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
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
            media: crate::now_playing::shared(),
            sse_clients: Default::default(),
            wallpaper_ready_ms: Default::default(),
            renderer_diag: Default::default(),
        };
        let html =
            b"<!DOCTYPE html><html><head><meta charset=utf-8></head><body></body></html>".to_vec();
        let out = inject_we_shim(&state, "wpem-inject-test-item", html);
        let s = String::from_utf8(out).unwrap();
        // prelude 拼在 <head …> 之后、</head> 之前
        let (before, after) = s.split_once("</head>").unwrap();
        assert!(
            before.contains("__weSeedProps"),
            "种子属性应在 </head> 之前"
        );
        assert!(
            before.contains("__weSetFps"),
            "shim 控制接口应在 </head> 之前"
        );
        assert!(after.contains("<body>"));
        // 属性值里的标签结束序列被 JSON unicode 转义（解析回原文本，且不会提前闭合标签）
        assert!(s.contains(r"a\u003c/script>b"), "值中的结束标签已转义");
        assert!(!s.contains("a</script>b"), "原文中的结束标签不应原样出现");
        // 种子脚本只出现一次（一次注入，不会叠加）
        assert_eq!(s.matches("window.__weSeedProps(").count(), 1);
    }
}

/// 解析 "bytes=start-end"（单区间，开口端按 total 补齐）。不合法/越界返回 None
///（调用方回 416）。
fn parse_range(range: &str, total: u64) -> Option<(u64, u64)> {
    let spec = range.strip_prefix("bytes=")?;
    let spec = spec.split(',').next()?.trim();
    let (s, e) = spec.split_once('-')?;
    let start: u64 = s.parse().unwrap_or(0);
    let end: u64 = if e.is_empty() {
        total.saturating_sub(1)
    } else {
        e.parse()
            .unwrap_or(total.saturating_sub(1))
            .min(total.saturating_sub(1))
    };
    (start <= end && start < total).then_some((start, end))
}

/// 流式文件服务（视频等大文件）：Range 只 seek + 读请求区间，完整请求也流式写，
/// 全程不把整个文件读进内存。WebKit 播放期发大量小段 Range 请求，这是视频
/// 壁纸流畅播放（尤其循环交接处备用元素取数）的关键路径。
async fn serve_file_stream(
    stream: &mut tokio::net::TcpStream,
    file: &Path,
    mime: &str,
    range: &str,
) -> Result<(), String> {
    let total = tokio::fs::metadata(file)
        .await
        .map_err(|e| e.to_string())?
        .len();
    let mut f = tokio::fs::File::open(file)
        .await
        .map_err(|e| e.to_string())?;
    if range.starts_with("bytes=") {
        if let Some((start, end)) = parse_range(range, total) {
            {
                let len = end - start + 1;
                f.seek(std::io::SeekFrom::Start(start))
                    .await
                    .map_err(|e| e.to_string())?;
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: {mime}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
                );
                stream
                    .write_all(head.as_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
                let mut limited = f.take(len);
                tokio::io::copy(&mut limited, stream)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(());
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
    // 无 Range 的完整请求同样流式写（预览可能整取大图/大文件）
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nAccess-Control-Allow-Origin: *\r\nAccept-Ranges: bytes\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    tokio::io::copy(&mut f, stream)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
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

/// 扩展名 → Content-Type。
///
/// 缺一个类型的后果不只是"标注不准"：`XMLHttpRequest.responseXML` 按规范只在
/// Content-Type 属于 XML 类型时才解析，落到 `application/octet-stream` 会拿到
/// null。实测 3406740580（pano2vr 全景壁纸）就是这样整张不显示 —— 它用
/// responseXML 读 pano.xml 拿全景配置，解析不出来就建不起场景，而 HTTP 全是
/// 200、控制台也不报错，只有画面空着（本地静态服务器把 .xml 标成
/// application/xml，所以同一份文件在上游 bench 里正常）。
///
/// 所以这里宁可多列几个：**会被脚本按类型消费**的文本/XML/字幕/字体尤其不能漏。
/// 真正该保持 octet-stream 的是 pkg / tex 那类私有二进制。
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
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        // XML 家族：responseXML / DOMParser 依赖它，漏了会让配置驱动的壁纸空屏
        "xml" => "text/xml; charset=utf-8",
        "svg" => "image/svg+xml",
        // 纯文本：作者常用 .txt/.csv 存配置或歌词
        "txt" | "csv" | "md" => "text/plain; charset=utf-8",
        "vtt" => "text/vtt",
        "wasm" => "application/wasm",
        "pkg" => "application/octet-stream",
        "tex" => "application/octet-stream",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
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
