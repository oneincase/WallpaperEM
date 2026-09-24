//! MCP（Model Context Protocol）服务：让 agent 连上本应用做壁纸。
//!
//! 传输：Streamable HTTP，端点 `POST/GET/DELETE /mcp`，只绑回环地址。
//! - `POST`：JSON-RPC 2.0（单请求或 batch），一律回 `application/json` —— 本服务
//!   不主动向客户端推消息，因此不提供 SSE 响应体（规范允许服务端二选一）；
//! - `GET`：返回 405（规范允许：服务端不提供 SSE 流时回 405）；
//! - `DELETE`：会话结束，回 204。
//!
//! 鉴权：`?token=` 或 `Authorization: Bearer <token>`，两者取其一即可；
//! 额外拒绝「带浏览器 Origin 且不是回环」的请求，防 DNS rebinding。
//!
//! 默认关闭：开关/端口/token 都存在 settings 表，设置页改完热重启。

mod protocol;
mod tools;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use rusqlite::Connection;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::db;

/// 默认端口：可配置，选一个不常见的号段避免和开发服务打架
pub const DEFAULT_PORT: u16 = 7411;
const SETTING_ENABLED: &str = "mcp.enabled";
const SETTING_PORT: &str = "mcp.port";
const SETTING_TOKEN: &str = "mcp.token";
/// 调用日志保留条数（只留在内存里，设置页展示用）
const MAX_CALL_LOG: usize = 50;

/// 支持的 MCP 协议版本（新 → 旧）；客户端请求的版本不在表里时回最新的
pub const SUPPORTED_PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// 一条工具调用记录（设置页的「最近调用」）
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallLog {
    pub tool: String,
    pub ok: bool,
    pub ms: u64,
    pub summary: String,
    pub at: u64,
}

pub struct McpState {
    pub enabled: AtomicBool,
    pub port: Mutex<u16>,
    pub token: Mutex<String>,
    pub running: AtomicBool,
    pub last_error: Mutex<Option<String>>,
    pub session_id: Mutex<String>,
    pub calls: Mutex<VecDeque<CallLog>>,
    pub server: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl McpState {
    fn new(enabled: bool, port: u16, token: String) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            port: Mutex::new(port),
            token: Mutex::new(token),
            running: AtomicBool::new(false),
            last_error: Mutex::new(None),
            session_id: Mutex::new(random_hex(16)),
            calls: Mutex::new(VecDeque::new()),
            server: Mutex::new(None),
        }
    }

    pub fn token(&self) -> String {
        self.token.lock().map(|t| t.clone()).unwrap_or_default()
    }

    pub fn port(&self) -> u16 {
        self.port.lock().map(|p| *p).unwrap_or(DEFAULT_PORT)
    }

    fn push_log(&self, entry: CallLog) {
        if let Ok(mut log) = self.calls.lock() {
            log.push_front(entry);
            while log.len() > MAX_CALL_LOG {
                log.pop_back();
            }
        }
    }
}

// ---------------------------------------------------------------- 设置读写

fn conn(app: &AppHandle) -> Result<std::sync::Arc<Mutex<Connection>>, String> {
    app.try_state::<std::sync::Arc<Mutex<Connection>>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| "DB 未就绪".to_string())
}

fn load_settings(app: &AppHandle) -> (bool, u16, String) {
    let Ok(db) = conn(app) else {
        return (false, DEFAULT_PORT, random_hex(16));
    };
    let Ok(c) = db.lock() else {
        return (false, DEFAULT_PORT, random_hex(16));
    };
    let enabled = db::get_setting(&c, SETTING_ENABLED).as_deref() == Some("1");
    let port = db::get_setting(&c, SETTING_PORT)
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p > 1024)
        .unwrap_or(DEFAULT_PORT);
    let token = db::get_setting(&c, SETTING_TOKEN).unwrap_or_default();
    let token = if token.trim().is_empty() {
        let t = random_hex(16);
        let _ = db::set_setting(&c, SETTING_TOKEN, &t);
        t
    } else {
        token
    };
    (enabled, port, token)
}

fn save_setting(app: &AppHandle, key: &str, value: &str) -> Result<(), String> {
    let db = conn(app)?;
    let c = db.lock().map_err(|e| e.to_string())?;
    db::set_setting(&c, key, value)
}

fn random_hex(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes * 2);
    let mut buf = [0u8; 8];
    let mut left = bytes;
    while left > 0 {
        // 用系统时间 + 栈地址做种子；MCP token 只需本机不可猜，不要求密码学强度
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
            ^ (&buf as *const _ as u64).rotate_left(17)
            ^ (out.len() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut x = seed | 1;
        for slot in buf.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *slot = (x & 0xff) as u8;
        }
        for b in buf.iter() {
            if left == 0 {
                break;
            }
            out.push_str(&format!("{b:02x}"));
            left -= 1;
        }
    }
    out
}

// ---------------------------------------------------------------- 生命周期

pub fn init(app: &AppHandle) -> Result<(), String> {
    let (enabled, port, token) = load_settings(app);
    app.manage(McpState::new(enabled, port, token));
    if enabled {
        if let Err(e) = start(app) {
            tracing::warn!("MCP 服务启动失败: {e}");
            if let Some(st) = app.try_state::<McpState>() {
                *st.last_error.lock().unwrap() = Some(e);
            }
        }
    }
    Ok(())
}

/// 绑定回环监听端口。
///
/// 对 `AddrInUse` 做**有界**重试而不是一次就报「端口被占用」：`stop()` 只是 abort
/// 旧任务，监听套接字是异步释放的；而且 std 的 bind 不开 SO_REUSEADDR，只要该端口上
/// 还留着上一次服务留下的 TIME_WAIT 连接（改端口再改回来、刚跑过一次请求就轮换令牌），
/// 立刻 bind 也会拿到 EADDRINUSE。重试上限 ~400ms：真被别的进程占用时也会在这点时间内
/// 明确报错，不会把设置页卡住。
fn bind_loopback(port: u16) -> Result<std::net::TcpListener, String> {
    const ATTEMPTS: u32 = 8;
    let mut last = String::new();
    for i in 0..ATTEMPTS {
        match bind_loopback_once(port) {
            Ok(l) => return Ok(l),
            Err(e) => {
                last = e.to_string();
                // 只有「端口被占」值得等：权限/地址非法之类重试多少次都一样
                if e.kind() != std::io::ErrorKind::AddrInUse || i + 1 == ATTEMPTS {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    Err(format!("端口 {port} 绑定失败: {last}"))
}

/// 单次绑定。Unix 上开 SO_REUSEADDR：重启应用 / 改端口 / 轮换令牌后的快速重绑
/// 常撞上旧监听留下的 TIME_WAIT 连接（macOS 上残留 15~30s，比下面的重试窗口长
/// 一个量级）——std 的 bind 不开这个选项就必报「端口被占用」，这就是
/// 「改啥端口都被占用」的根源。SO_REUSEADDR 只放行 TIME_WAIT/无主残留，
/// 不能绑走别的进程正在 LISTEN 的端口，安全。
fn bind_loopback_once(port: u16) -> std::io::Result<std::net::TcpListener> {
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    #[cfg(unix)]
    {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )?;
        socket.set_reuse_address(true)?;
        socket.bind(&addr.into())?;
        socket.listen(128)?;
        Ok(socket.into())
    }
    #[cfg(not(unix))]
    {
        std::net::TcpListener::bind(addr)
    }
}

/// 启动 HTTP 服务。端口被占用等失败会写进状态（设置页可见），不 panic。
///
/// 绑定用同步 `std::net::TcpListener`：`start` 会被 `init`（Tauri setup 钩子，主线程、
/// **没有 Tokio 运行时上下文**）与设置页命令两条路径调用，同步 bind 才能把「端口被占用」
/// 当场报回去。转成异步监听必须在 `tauri::async_runtime::spawn` 出来的任务里做 ——
/// `tokio::net::TcpListener::from_std` 要求当前线程挂着 reactor，在 setup 钩子里直接调会
/// panic「there is no reactor running」。
fn start(app: &AppHandle) -> Result<(), String> {
    let Some(st) = app.try_state::<McpState>() else {
        return Err("MCP 状态未初始化".into());
    };
    stop(app);
    let port = st.port();
    let listener = bind_loopback(port)?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("设置非阻塞失败: {e}"))?;

    let app2 = app.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("MCP 监听转入异步失败: {e}");
                if let Some(st) = app2.try_state::<McpState>() {
                    st.running.store(false, Ordering::Relaxed);
                    *st.last_error.lock().unwrap() = Some(format!("转入异步监听失败: {e}"));
                }
                return;
            }
        };
        let router = Router::new()
            .route("/mcp", post(handle_post).get(handle_get).delete(handle_delete))
            .with_state(app2.clone());
        tracing::info!("MCP server listening on http://127.0.0.1:{port}/mcp");
        if let Err(e) = axum::serve(listener, router).await {
            tracing::warn!("MCP server stopped: {e}");
            if let Some(st) = app2.try_state::<McpState>() {
                st.running.store(false, Ordering::Relaxed);
                *st.last_error.lock().unwrap() = Some(format!("服务异常退出: {e}"));
            }
        }
    });
    *st.server.lock().unwrap() = Some(handle);
    st.running.store(true, Ordering::Relaxed);
    *st.last_error.lock().unwrap() = None;
    Ok(())
}

fn stop(app: &AppHandle) {
    if let Some(st) = app.try_state::<McpState>() {
        if let Some(h) = st.server.lock().unwrap().take() {
            h.abort();
        }
        st.running.store(false, Ordering::Relaxed);
    }
}

fn restart(app: &AppHandle) {
    let enabled = app
        .try_state::<McpState>()
        .map(|s| s.enabled.load(Ordering::Relaxed))
        .unwrap_or(false);
    if !enabled {
        stop(app);
        return;
    }
    if let Err(e) = start(app) {
        tracing::warn!("MCP 服务重启失败: {e}");
        if let Some(st) = app.try_state::<McpState>() {
            st.running.store(false, Ordering::Relaxed);
            *st.last_error.lock().unwrap() = Some(e);
        }
    }
}

// ---------------------------------------------------------------- 状态 / 命令

fn status_value(app: &AppHandle) -> Value {
    let Some(st) = app.try_state::<McpState>() else {
        return json!({ "available": false });
    };
    let port = st.port();
    let token = st.token();
    let calls: Vec<CallLog> = st
        .calls
        .lock()
        .map(|c| c.iter().cloned().collect())
        .unwrap_or_default();
    json!({
        "available": true,
        "enabled": st.enabled.load(Ordering::Relaxed),
        "running": st.running.load(Ordering::Relaxed),
        "port": port,
        "token": token,
        "url": format!("http://127.0.0.1:{port}/mcp"),
        "urlWithToken": format!("http://127.0.0.1:{port}/mcp?token={token}"),
        "lastError": st.last_error.lock().ok().and_then(|e| e.clone()),
        "calls": calls,
    })
}

pub fn config_snippet_value(app: &AppHandle) -> Value {
    let st = app.try_state::<McpState>();
    let (port, token) = st
        .map(|s| (s.port(), s.token()))
        .unwrap_or((DEFAULT_PORT, String::new()));
    let url = format!("http://127.0.0.1:{port}/mcp?token={token}");
    json!({
        "url": url,
        "json": serde_json::to_string_pretty(&json!({
            "mcpServers": { "wallpaperem": { "url": url } }
        })).unwrap_or_default(),
        "codexCli": format!("codex mcp add wallpaperem --url {url}"),
        "claudeCli": format!("claude mcp add --transport http wallpaperem {url}"),
    })
}

#[tauri::command]
pub fn mcp_status(app: AppHandle) -> Value {
    status_value(&app)
}

#[tauri::command]
pub fn mcp_set_enabled(app: AppHandle, enabled: bool) -> Result<Value, String> {
    save_setting(&app, SETTING_ENABLED, if enabled { "1" } else { "0" })?;
    if let Some(st) = app.try_state::<McpState>() {
        st.enabled.store(enabled, Ordering::Relaxed);
    }
    restart(&app);
    Ok(status_value(&app))
}

#[tauri::command]
pub fn mcp_set_port(app: AppHandle, port: u16) -> Result<Value, String> {
    if port <= 1024 {
        return Err("端口需大于 1024（避开系统保留端口）".into());
    }
    save_setting(&app, SETTING_PORT, &port.to_string())?;
    if let Some(st) = app.try_state::<McpState>() {
        *st.port.lock().unwrap() = port;
    }
    restart(&app);
    Ok(status_value(&app))
}

#[tauri::command]
pub fn mcp_rotate_token(app: AppHandle) -> Result<Value, String> {
    let token = random_hex(16);
    save_setting(&app, SETTING_TOKEN, &token)?;
    if let Some(st) = app.try_state::<McpState>() {
        *st.token.lock().unwrap() = token;
        *st.session_id.lock().unwrap() = random_hex(16);
    }
    // 不重启：令牌是每个请求现读的（authorize → st.token()），换值即刻生效。
    // 重启反而要重绑同一个端口，白白掐断正在处理的请求、还可能撞上 TIME_WAIT。
    Ok(status_value(&app))
}

#[tauri::command]
pub fn mcp_config_snippet(app: AppHandle) -> Value {
    config_snippet_value(&app)
}

// ---------------------------------------------------------------- HTTP

fn json_response(status: StatusCode, body: &Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// 定长比较，避免用 == 比 token 时泄露前缀长度信息
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    let h = h.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or(h);
    matches!(h.as_str(), "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

/// 鉴权 + DNS rebinding 防护。通过返回 Ok，否则返回可直接回给客户端的 Response。
fn authorize(
    st: &McpState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Result<(), Response> {
    // 浏览器页面发起的跨站请求会带 Origin；本服务只服务本机 MCP 客户端，
    // 非回环 Origin 一律拒绝（否则任意网页都能借 DNS rebinding 摸到本服务）。
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let host = origin
            .split("//")
            .nth(1)
            .unwrap_or("")
            .trim_end_matches('/');
        if !host.is_empty() && !is_loopback_host(host) {
            return Err(json_response(
                StatusCode::FORBIDDEN,
                &rpc_error(Value::Null, -32001, "Origin 不是本机回环，已拒绝"),
            ));
        }
    }
    let provided = query
        .get("token")
        .map(|s| s.as_str())
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        })
        .unwrap_or("");
    if !ct_eq(provided, &st.token()) {
        return Err(json_response(
            StatusCode::UNAUTHORIZED,
            &rpc_error(Value::Null, -32001, "缺少或错误的 token"),
        ));
    }
    Ok(())
}

async fn handle_post(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let Some(st) = app.try_state::<McpState>() else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &rpc_error(Value::Null, -32603, "MCP 状态未就绪"),
        );
    };
    if let Err(resp) = authorize(&st, &headers, &query) {
        return resp;
    }
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &rpc_error(Value::Null, -32700, &format!("JSON 解析失败: {e}")),
            )
        }
    };
    let session = st.session_id.lock().map(|s| s.clone()).unwrap_or_default();

    let response = match parsed {
        Value::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                if let Some(v) = protocol::handle_message(&app, &st, item).await {
                    out.push(v);
                }
            }
            if out.is_empty() {
                return (
                    StatusCode::ACCEPTED,
                    [(header::CONTENT_TYPE, "application/json")],
                    "{}",
                )
                    .into_response();
            }
            Value::Array(out)
        }
        other => match protocol::handle_message(&app, &st, other).await {
            Some(v) => v,
            // 通知类消息没有响应体：规范要求回 202 Accepted
            None => {
                return (
                    StatusCode::ACCEPTED,
                    [(header::CONTENT_TYPE, "application/json")],
                    "{}",
                )
                    .into_response()
            }
        },
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (
                header::HeaderName::from_static("mcp-session-id"),
                session.as_str(),
            ),
        ],
        response.to_string(),
    )
        .into_response()
}

/// 本服务不向客户端推消息 → 不提供 SSE 流（规范允许回 405）
async fn handle_get(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(st) = app.try_state::<McpState>() else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &rpc_error(Value::Null, -32603, "MCP 状态未就绪"),
        );
    };
    if let Err(resp) = authorize(&st, &headers, &query) {
        return resp;
    }
    json_response(
        StatusCode::METHOD_NOT_ALLOWED,
        &rpc_error(
            Value::Null,
            -32601,
            "本服务不提供服务端事件流（无服务端推送），请用 POST 发送请求",
        ),
    )
}

async fn handle_delete(
    State(app): State<AppHandle>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(st) = app.try_state::<McpState>() else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &rpc_error(Value::Null, -32603, "MCP 状态未就绪"),
        );
    };
    if let Err(resp) = authorize(&st, &headers, &query) {
        return resp;
    }
    // 会话无服务端状态（每个请求自带上下文），删除即重新生成会话 id
    if let Ok(mut s) = st.session_id.lock() {
        *s = random_hex(16);
    }
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_is_exact() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
        assert!(!ct_eq("", "a"));
    }

    #[test]
    fn loopback_hosts() {
        assert!(is_loopback_host("127.0.0.1:7411"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("[::1]:7411"));
        assert!(!is_loopback_host("evil.com"));
        assert!(!is_loopback_host("127.0.0.1.evil.com"));
    }

    #[test]
    fn random_hex_shape() {
        let t = random_hex(16);
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t, random_hex(16));
    }

    /// Token 与 Origin 两道闸门是这服务唯一的对外防线（只绑回环 + 令牌），
    /// authorize 不碰 AppHandle，可以脱离运行中的应用单测。
    #[test]
    fn authorize_enforces_token_and_origin() {
        let st = McpState::new(true, 7411, "secret-token".into());
        let no_query = HashMap::new();
        let query = |t: &str| HashMap::from([("token".to_string(), t.to_string())]);
        let headers = |origin: Option<&str>, bearer: Option<&str>| {
            let mut m = HeaderMap::new();
            if let Some(o) = origin {
                m.insert(header::ORIGIN, o.parse().unwrap());
            }
            if let Some(b) = bearer {
                m.insert(header::AUTHORIZATION, format!("Bearer {b}").parse().unwrap());
            }
            m
        };

        // 没有令牌 / 令牌不对 → 401
        assert_eq!(
            authorize(&st, &headers(None, None), &no_query)
                .unwrap_err()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            authorize(&st, &headers(None, None), &query("wrong"))
                .unwrap_err()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        // 空令牌不能因为「服务端令牌恰好非空」而蒙混过关
        assert!(authorize(&st, &headers(None, Some("")), &no_query).is_err());

        // 两种携带方式都要认
        assert!(authorize(&st, &headers(None, None), &query("secret-token")).is_ok());
        assert!(authorize(&st, &headers(None, Some("secret-token")), &no_query).is_ok());

        // 浏览器带 Origin：回环放行，外部域名拒绝（DNS rebinding 防线）
        assert!(authorize(
            &st,
            &headers(Some("http://127.0.0.1:7411"), None),
            &query("secret-token")
        )
        .is_ok());
        assert_eq!(
            authorize(
                &st,
                &headers(Some("https://evil.example.com"), None),
                &query("secret-token")
            )
            .unwrap_err()
            .status(),
            StatusCode::FORBIDDEN
        );
        // 令牌正确但 Origin 非回环，仍然拒绝（顺序上 Origin 先判）
        assert_eq!(
            authorize(
                &st,
                &headers(Some("http://127.0.0.1.evil.com"), None),
                &no_query
            )
            .unwrap_err()
            .status(),
            StatusCode::FORBIDDEN
        );
    }
}
