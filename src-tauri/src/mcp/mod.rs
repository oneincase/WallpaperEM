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

mod api;
mod protocol;
pub(crate) mod shares;
mod tools;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use rusqlite::Connection;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::db;

/// 默认端口：可配置，选一个不常见的号段避免和开发服务打架
pub const DEFAULT_PORT: u16 = 7411;
const SETTING_ENABLED: &str = "mcp.enabled";
/// 总开关的新键（v1.2 网络服务改造）：MCP + REST API + 分享共用一个服务。
/// 旧键 `mcp.enabled` 只作迁移源 —— 新键缺失时读旧键，写入一律落新键
const SETTING_SERVICE_ENABLED: &str = "service.enabled";
const SETTING_NET_MODE: &str = "service.net_mode";
const SETTING_PORT: &str = "mcp.port";
const SETTING_TOKEN: &str = "mcp.token";
/// 调用日志保留条数（只留在内存里，设置页展示用）
const MAX_CALL_LOG: usize = 50;

/// 网络模式：服务监听的位置与可访问范围（见 docs/network-service-and-sharing.md §4）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NetMode {
    /// 本机：只绑回环，现状行为
    #[default]
    Loopback,
    /// 局域网：绑 0.0.0.0 + 中间件按源 IP 过滤（只放行私网/链路本地/回环）
    Lan,
    /// 任意：绑 0.0.0.0 不过滤（公网可达与否取决于路由器/防火墙）
    Any,
}

impl NetMode {
    fn from_setting(v: Option<&str>) -> Self {
        match v {
            Some("lan") => NetMode::Lan,
            Some("any") => NetMode::Any,
            _ => NetMode::Loopback,
        }
    }
    fn as_str(&self) -> &'static str {
        match self {
            NetMode::Loopback => "loopback",
            NetMode::Lan => "lan",
            NetMode::Any => "any",
        }
    }
    /// 各模式下服务进程实际监听的地址
    fn bind_addr(&self) -> std::net::Ipv4Addr {
        match self {
            NetMode::Loopback => std::net::Ipv4Addr::LOCALHOST,
            NetMode::Lan | NetMode::Any => std::net::Ipv4Addr::UNSPECIFIED,
        }
    }
}

/// 局域网模式放行的源地址：回环 / RFC1918 私网 / 链路本地 / IPv6 ULA 与链路本地。
/// 「只绑内网网卡 IP」在多网卡/DHCP 换 IP 下会悄悄失效，绑 0.0.0.0 + 应用层过滤
/// 在任意网络环境下语义稳定（方案 §4.1）。
fn peer_allowed_in_lan(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // 链路本地 fe80::/10
                || v6.is_unspecified()
        }
    }
}

/// 本机在局域网的地址（设置页展示 / 分享链接 host / Origin 校验共用）。
///
/// 枚举网卡，取**物理网卡上的私网 IPv4**：默认路由可能走 VPN 隧道（utun
/// fake-ip 网段 198.18.0.0/15，挂代理的机器上 UDP 默认路由探测会拿到无用的
/// 隧道地址），所以按接口名过滤虚拟网卡，en*/eth*/wl* 优先；找不到再退回
/// UDP 默认路由探测，最后返回 None 让调用方隐藏相关 UI。
pub(crate) fn primary_lan_ip() -> Option<std::net::IpAddr> {
    const VIRTUAL_IF_PREFIXES: [&str; 10] = [
        "lo", "utun", "tun", "tap", "bridge", "awdl", "llw", "ap", "docker", "veth",
    ];
    let preferred = |name: &str| {
        name.starts_with("en") || name.starts_with("eth") || name.starts_with("wl")
    };
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        let mut fallback: Option<std::net::IpAddr> = None;
        for i in ifs {
            let name = i.name.to_ascii_lowercase();
            if VIRTUAL_IF_PREFIXES.iter().any(|p| name.starts_with(p)) {
                continue;
            }
            if let std::net::IpAddr::V4(v4) = i.ip() {
                if v4.is_loopback() || !v4.is_private() {
                    continue;
                }
                let ip = std::net::IpAddr::V4(v4);
                if preferred(&name) {
                    return Some(ip);
                }
                if fallback.is_none() {
                    fallback = Some(ip);
                }
            }
        }
        if fallback.is_some() {
            return fallback;
        }
    }
    // 兜底：UDP connect 探测默认路由出口（不发任何包）。挂 VPN 时会拿到隧道
    // 地址 —— 只在网卡枚举完全失败时才退到这条路
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    s.local_addr().ok().map(|a| a.ip())
}

/// Origin/Host 头的 host 部分是否指向本机（回环或本机在局域网的地址）。
/// 只认字面量地址：主机名场景（macbook.local）同源访问不带 Origin，不在放行之列。
fn is_local_host_name(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    let h = h.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or(h);
    let h = h.trim_start_matches('[').trim_end_matches(']');
    match h.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback() || primary_lan_ip().is_some_and(|p| p == ip),
        Err(_) => false,
    }
}

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
    /// 网络模式（热读：每个请求现取，设置改动即时生效；绑定地址变更靠 restart）
    pub net_mode: Mutex<NetMode>,
    pub running: AtomicBool,
    pub last_error: Mutex<Option<String>>,
    pub session_id: Mutex<String>,
    pub calls: Mutex<VecDeque<CallLog>>,
    pub server: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl McpState {
    fn new(enabled: bool, port: u16, token: String, net_mode: NetMode) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            port: Mutex::new(port),
            token: Mutex::new(token),
            net_mode: Mutex::new(net_mode),
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

    pub(crate) fn net_mode(&self) -> NetMode {
        self.net_mode
            .lock()
            .map(|m| *m)
            .unwrap_or(NetMode::Loopback)
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

fn load_settings(app: &AppHandle) -> (bool, u16, String, NetMode) {
    let Ok(db) = conn(app) else {
        return (false, DEFAULT_PORT, random_hex(16), NetMode::default());
    };
    let Ok(c) = db.lock() else {
        return (false, DEFAULT_PORT, random_hex(16), NetMode::default());
    };
    // 总开关：新键优先，缺失回落旧键（老版本升级迁移），都没写 = 关
    let enabled = match db::get_setting(&c, SETTING_SERVICE_ENABLED)
        .or_else(|| db::get_setting(&c, SETTING_ENABLED))
    {
        Some(v) => v == "1" || v == "true",
        None => false,
    };
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
    let net_mode = NetMode::from_setting(db::get_setting(&c, SETTING_NET_MODE).as_deref());
    (enabled, port, token, net_mode)
}

fn save_setting(app: &AppHandle, key: &str, value: &str) -> Result<(), String> {
    let db = conn(app)?;
    {
        let c = db.lock().map_err(|e| e.to_string())?;
        db::set_setting(&c, key, value)?;
    }
    // MCP 侧改的共享设置：托盘勾选与各窗口控件同步跟上
    crate::notify_setting_changed(app, key, value);
    Ok(())
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
    let (enabled, port, token, net_mode) = load_settings(app);
    app.manage(McpState::new(enabled, port, token, net_mode));
    spawn_share_cleanup(app.clone());
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

/// 过期分享清扫：访问侧由 `shares::lookup_live` 懒过期保证正确性，这里只回收
/// DB 行（10 分钟一轮，无行可删时是零成本查询）
fn spawn_share_cleanup(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(600));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if let Some(db) = app.try_state::<std::sync::Arc<Mutex<Connection>>>() {
                if let Ok(conn) = db.lock() {
                    match conn.execute(
                        "DELETE FROM shares WHERE expires_at IS NOT NULL AND expires_at <= ?1",
                        [now],
                    ) {
                        Ok(n) if n > 0 => tracing::info!("清扫了 {n} 条过期分享"),
                        Ok(_) => {}
                        Err(e) => tracing::warn!("过期分享清扫失败: {e}"),
                    }
                }
            }
        }
    });
}

/// 分享域的 SSE 订阅桩：token 必须是活着的分享（防止探测），之后回 204 ——
/// EventSource 对非 2xx/非 event-stream 的响应按致命失败处理，不再重连
async fn share_sse_stub(
    State(app): State<AppHandle>,
    Path(token): Path<String>,
) -> Response {
    match shares::lookup_live(&app, &token) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(resp) => resp,
    }
}

/// 渲染器诊断回流（桌面壁纸窗口走内容服务器的 /diag；分享页落在这里）：
/// 只记日志，不打断渲染器
async fn diag_sink(Query(query): Query<HashMap<String, String>>) -> Response {
    if let Some(msg) = query.get("msg") {
        tracing::debug!("[share renderer diag] {msg}");
    }
    StatusCode::NO_CONTENT.into_response()
}

/// 渲染器页与静态资源（分享访客在浏览器里需要它；桌面壁纸窗口走内容服务器，
/// 两边是同一份渲染器代码、同一套 query 契约）。
///
/// dev → 代理 vite dev server（localhost:1420，与 tauri.conf devUrl 一致；本地
/// 直连绝不走系统代理，否则渲染页/媒体加载失败——内容服务器同款处理）。
/// prod → 资源目录（tauri.conf 把 dist/renderer 打包成 `<res>/renderer`），
/// vite 产物里的 `/assets/*` 绝对引用对应 `<res>/assets`。
async fn renderer_page(State(app): State<AppHandle>, req: axum::extract::Request) -> Response {
    // dev 分支不用句柄（代理 vite），prod 分支取资源目录 —— 统一压一次引用消警告
    let _ = &app;
    let path = req.uri().path().to_string();
    let query = req
        .uri()
        .query()
        .map(|s| format!("?{s}"))
        .unwrap_or_default();
    #[cfg(debug_assertions)]
    {
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|e| e.to_string())
            .and_then(|c| Ok(c));
        let client = match client {
            Ok(c) => c,
            Err(e) => {
                return (StatusCode::BAD_GATEWAY, format!("proxy client error: {e}")).into_response()
            }
        };
        // 去掉前导斜杠再拼：`//renderer/…` 会被 vite 当 SPA fallback 回 index.html
        // （text/html），模块脚本的 MIME 就错了 —— 内容服务器同款处理
        let vite_path = path.trim_start_matches('/');
        let upstream = format!("http://localhost:1420/{vite_path}{query}");
        return match client.get(&upstream).send().await {
            Ok(resp) => {
                let status = StatusCode::from_u16(resp.status().as_u16())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let ct = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let body = resp.bytes().await.unwrap_or_default();
                (
                    status,
                    [(header::CONTENT_TYPE, ct)],
                    body,
                )
                    .into_response()
            }
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                format!("vite proxy error: {e}"),
            )
                .into_response(),
        };
    }
    #[cfg(not(debug_assertions))]
    {
        let res = app
            .path()
            .resource_dir()
            .unwrap_or_default();
        let (dir, rel) = if path == "/renderer" || path == "/renderer/" {
            (res.join("renderer"), "index.html".to_string())
        } else if let Some(p) = path.strip_prefix("/renderer/") {
            (res.join("renderer"), p.to_string())
        } else if let Some(p) = path.strip_prefix("/assets/") {
            (res.join("assets"), p.to_string())
        } else {
            return (StatusCode::NOT_FOUND, "Not Found").into_response();
        };
        shares::serve_from(dir, rel, req).await
    }
}

/// 绑定监听端口（地址按网络模式：回环 / 全接口）。
///
/// 对 `AddrInUse` 做**有界**重试而不是一次就报「端口被占用」：`stop()` 只是 abort
/// 旧任务，监听套接字是异步释放的；而且 std 的 bind 不开 SO_REUSEADDR，只要该端口上
/// 还留着上一次服务留下的 TIME_WAIT 连接（改端口再改回来、刚跑过一次请求就轮换令牌），
/// 立刻 bind 也会拿到 EADDRINUSE。重试上限 ~400ms：真被别的进程占用时也会在这点时间内
/// 明确报错，不会把设置页卡住。
fn bind_listener(port: u16, addr: std::net::Ipv4Addr) -> Result<std::net::TcpListener, String> {
    const ATTEMPTS: u32 = 8;
    let mut last = String::new();
    for i in 0..ATTEMPTS {
        match bind_listener_once(port, addr) {
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
fn bind_listener_once(port: u16, addr: std::net::Ipv4Addr) -> std::io::Result<std::net::TcpListener> {
    let socket_addr: std::net::SocketAddr = (addr, port).into();
    #[cfg(unix)]
    {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )?;
        socket.set_reuse_address(true)?;
        socket.bind(&socket_addr.into())?;
        socket.listen(128)?;
        Ok(socket.into())
    }
    #[cfg(not(unix))]
    {
        std::net::TcpListener::bind(socket_addr)
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
    let mode = st.net_mode();
    let listener = bind_listener(port, mode.bind_addr())?;
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
        // 源过滤层：局域网模式按源 IP 放行（回环模式下监听地址本身已是 127.0.0.1，
        // 一律本地，层直通）；整棵路由树统一生效 —— /api、/share、/docs 全部继承。
        // 路由族：/mcp（JSON-RPC）、/api/v1（REST 控制面）、/share（分享访客面）、
        // /docs + /renderer（分享页需要的静态资源，dev 代理 vite / prod 资源目录）。
        let router = Router::new()
            .route("/mcp", post(handle_post).get(handle_get).delete(handle_delete))
            .merge(api::router())
            .route("/docs", get(api::docs_page))
            .merge(shares::router())
            .route("/renderer", get(renderer_page))
            .route("/renderer/{*path}", get(renderer_page))
            .route("/assets/{*path}", get(renderer_page));
        // dev：vite 把裸模块重写成 /node_modules/.vite/deps/… 等绝对路径，
        // 代理层必须放行（内容服务器同款处理）；prod 由 vite 打包进 /assets
        #[cfg(debug_assertions)]
        let router = router
            .route("/node_modules/{*path}", get(renderer_page))
            .route("/@vite/{*path}", get(renderer_page))
            .route("/@id/{*path}", get(renderer_page))
            // vite react 插件的 HMR 模块（与 /@vite 同族的虚拟路径）
            .route("/@react-refresh", get(renderer_page));
        let router = router
            // 分享页的 SSE 订阅（渲染器把 audioToken 复用为订阅令牌）：分享没有
            // 系统音频/正在播放数据，204 让 EventSource 按「致命失败」收场不重试 ——
            // 404 会触发它的自动重连循环刷请求
            .route("/audio-stream/{token}", get(share_sse_stub))
            .route("/now-playing/{token}", get(share_sse_stub))
            // 渲染器的诊断回流（分享域没有内容服务器的 /diag，这里落日志）
            .route("/diag", get(diag_sink).post(diag_sink));
        let router = router
            .layer(axum::middleware::from_fn_with_state(
                app2.clone(),
                net_guard,
            ))
            .with_state(app2.clone());
        let shown = match mode {
            NetMode::Loopback => format!("127.0.0.1:{port}"),
            _ => format!("0.0.0.0:{port} ({})", mode.as_str()),
        };
        tracing::info!("MCP server listening on http://{shown}/mcp");
        if let Err(e) = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        {
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
    let mode = st.net_mode();
    let calls: Vec<CallLog> = st
        .calls
        .lock()
        .map(|c| c.iter().cloned().collect())
        .unwrap_or_default();
    // 局域网/任意模式下给出局域网地址（分享链接与跨设备访问用）；本机模式为 null。
    // 探测不到出口 IP（离线/多网卡异常）时也回 null，前端隐藏该块而不是显示坏链接
    let lan_url = if mode != NetMode::Loopback {
        primary_lan_ip().map(|ip| format!("http://{ip}:{port}"))
    } else {
        None
    };
    json!({
        "available": true,
        "enabled": st.enabled.load(Ordering::Relaxed),
        "running": st.running.load(Ordering::Relaxed),
        "netMode": mode.as_str(),
        "port": port,
        "token": token,
        "url": format!("http://127.0.0.1:{port}/mcp"),
        "urlWithToken": format!("http://127.0.0.1:{port}/mcp?token={token}"),
        "lanUrl": lan_url,
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
    // 写新键 service.enabled（旧键 mcp.enabled 只在读取时作迁移源，不再回写）
    save_setting(&app, SETTING_SERVICE_ENABLED, if enabled { "1" } else { "0" })?;
    if let Some(st) = app.try_state::<McpState>() {
        st.enabled.store(enabled, Ordering::Relaxed);
    }
    restart(&app);
    Ok(status_value(&app))
}

/// 网络模式（本机/局域网/任意）。绑定地址随模式变化，必须热重启监听。
#[tauri::command]
pub fn mcp_set_net_mode(app: AppHandle, mode: String) -> Result<Value, String> {
    let m = match mode.as_str() {
        "loopback" => NetMode::Loopback,
        "lan" => NetMode::Lan,
        "any" => NetMode::Any,
        _ => return Err(format!("未知的网络模式: {mode}（可选 loopback/lan/any）")),
    };
    save_setting(&app, SETTING_NET_MODE, m.as_str())?;
    if let Some(st) = app.try_state::<McpState>() {
        *st.net_mode.lock().unwrap() = m;
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

/// 网络模式的源过滤层（整棵路由树统一生效）。
///
/// - 本机：监听地址就是回环，一切来源都是本地 —— 直通；
/// - 局域网：只放行私网/链路本地/回环来源（`peer_allowed_in_lan`），公网来源 403；
/// - 任意：不过滤（安全完全依赖 token；设置页已警示）。
async fn net_guard(
    State(app): State<AppHandle>,
    peer: axum::extract::ConnectInfo<std::net::SocketAddr>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mode = app
        .try_state::<McpState>()
        .map(|s| s.net_mode())
        .unwrap_or(NetMode::Loopback);
    if mode == NetMode::Lan && !peer_allowed_in_lan(peer.ip()) {
        return json_response(
            StatusCode::FORBIDDEN,
            &rpc_error(Value::Null, -32002, "非局域网来源，已拒绝"),
        );
    }
    next.run(req).await
}

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

/// 鉴权 + DNS rebinding 防护（按网络模式分级）。通过返回 Ok，否则返回可直接
/// 回给客户端的 Response。
///
/// Origin 闸门（浏览器页面发起的跨站请求会带 Origin）：
/// - 本机模式：非回环 Origin 一律拒绝（旧行为不变）；
/// - 局域网/任意：只认「本机地址」形态的 Origin（回环 / 本机局域网 IP）。DNS
///   rebinding 下 Origin 仍是攻击者域名、Host 才是被劫持的地址，二者必然不等，
///   因此**不能**用「Origin == Host」作判据；外部域名的浏览器请求在此被拒。
///   同源页面（落地页/直链）不带 Origin，不受影响。控制面没有跨源浏览器调用方
///   （API 消费者都是脚本/快捷指令，不带 Origin），这条收紧没有误伤面。
fn authorize(
    st: &McpState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Result<(), Response> {
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let host = origin
            .split("//")
            .nth(1)
            .unwrap_or("")
            .trim_end_matches('/');
        if !host.is_empty() {
            let ok = match st.net_mode() {
                NetMode::Loopback => is_loopback_host(host),
                NetMode::Lan | NetMode::Any => {
                    is_loopback_host(host) || is_local_host_name(host)
                }
            };
            if !ok {
                return Err(json_response(
                    StatusCode::FORBIDDEN,
                    &rpc_error(Value::Null, -32001, "Origin 不是本机地址，已拒绝"),
                ));
            }
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
    fn net_mode_parses_whitelist() {
        assert_eq!(NetMode::from_setting(Some("lan")), NetMode::Lan);
        assert_eq!(NetMode::from_setting(Some("any")), NetMode::Any);
        assert_eq!(NetMode::from_setting(Some("loopback")), NetMode::Loopback);
        // 旧值/手改 DB 回落本机
        assert_eq!(NetMode::from_setting(Some("bogus")), NetMode::Loopback);
        assert_eq!(NetMode::from_setting(None), NetMode::Loopback);
        // 绑定地址随模式变化
        assert_eq!(NetMode::Loopback.bind_addr().to_string(), "127.0.0.1");
        assert_eq!(NetMode::Lan.bind_addr().to_string(), "0.0.0.0");
    }

    #[test]
    fn lan_peer_filter_allows_private_rejects_public() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
        // 放行：回环 / RFC1918 / 链路本地 / ULA
        for ok in [
            "127.0.0.1", "10.0.0.5", "172.16.1.9", "192.168.1.100", "169.254.3.4",
        ] {
            assert!(
                peer_allowed_in_lan(ok.parse::<IpAddr>().unwrap()),
                "应放行 {ok}"
            );
        }
        assert!(peer_allowed_in_lan(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(peer_allowed_in_lan(
            "fe80::1".parse::<IpAddr>().unwrap(),
        ));
        assert!(peer_allowed_in_lan(
            "fc00::1234".parse::<IpAddr>().unwrap(),
        ));
        // 拒绝：公网来源
        for bad in ["8.8.8.8", "1.1.1.1", "172.32.0.1"] {
            assert!(
                !peer_allowed_in_lan(bad.parse::<IpAddr>().unwrap()),
                "应拒绝 {bad}"
            );
        }
    }

    #[test]
    fn local_origin_host_accepts_own_addresses_only() {
        // 回环与本机探测 IP 都算「本机地址」
        assert!(is_local_host_name("127.0.0.1:7411"));
        assert!(is_local_host_name("[::1]:7411"));
        if let Some(ip) = primary_lan_ip() {
            assert!(is_local_host_name(&format!("{ip}:7411")));
        }
        // 外部域名 / 相似域名 / 主机名形态一律不认（同源访问不带 Origin，无需放行）
        assert!(!is_local_host_name("evil.com"));
        assert!(!is_local_host_name("192.168.1.99:7411"));
        assert!(!is_local_host_name("my-macbook.local:7411"));
    }

    #[test]
    fn random_hex_shape() {
        let t = random_hex(16);
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t, random_hex(16));
    }

    /// Token 与 Origin 两道闸门是这服务唯一的对外防线（令牌 + 按模式的 Origin 策略），
    /// authorize 不碰 AppHandle，可以脱离运行中的应用单测。
    #[test]
    fn authorize_enforces_token_and_origin() {
        let st = McpState::new(true, 7411, "secret-token".into(), NetMode::Loopback);
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

    /// 局域网/任意模式：本机 IP 形态的 Origin 放行、外部域名拒绝（rebinding 时
    /// Origin 仍是攻击者域名，即使 Host 被劫持到本机地址）。
    #[test]
    fn authorize_origin_policy_follows_net_mode() {
        let mut st = McpState::new(true, 7411, "secret-token".into(), NetMode::Lan);
        let query = |t: &str| HashMap::from([("token".to_string(), t.to_string())]);
        let with_origin = |o: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::ORIGIN, o.parse().unwrap());
            m
        };

        // 回环 Origin 放行
        assert!(authorize(&st, &with_origin("http://127.0.0.1:7411"), &query("secret-token")).is_ok());
        // 本机局域网 IP 形态的 Origin 放行（有出口 IP 才可断言；无则跳过该分支）
        if let Some(ip) = primary_lan_ip() {
            assert!(authorize(
                &st,
                &with_origin(&format!("http://{ip}:7411")),
                &query("secret-token")
            )
            .is_ok());
        }
        // 外部域名拒绝
        assert_eq!(
            authorize(&st, &with_origin("https://evil.example.com"), &query("secret-token"))
                .unwrap_err()
                .status(),
            StatusCode::FORBIDDEN
        );
        // 任意模式同策略
        *st.net_mode.lock().unwrap() = NetMode::Any;
        assert_eq!(
            authorize(&st, &with_origin("https://evil.example.com"), &query("secret-token"))
                .unwrap_err()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
}
