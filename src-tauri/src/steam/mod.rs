//! Steam 客户端：统一 HTTP 客户端（代理 + 串行重试 + sessionid 反 403），对齐 Web 版 net.ts / steamFetch

pub mod browse;
pub mod details;
pub mod types;

use std::sync::{Arc, Mutex};
use std::time::Duration;
use reqwest::{Response, StatusCode};

pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";
const MAX_ATTEMPTS: u32 = 3;
/// 有 Steam 令牌/已登录时，匿名请求常被 Steam 社区返回 403。
/// 方案 A：请求前先 GET 一次 steamcommunity.com，从 Set-Cookie 取出 sessionid，
/// 之后所有工坊请求带上该 Cookie 以绕过 403。见 https://steamcommunity.com
const SESSION_URL: &str = "https://steamcommunity.com/";

/// 浏览器级完整请求头（Steam 会校验 Referer/Accept 等，缺省易触发 403/机器人校验）
fn browser_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::*;
    let mut h = HeaderMap::new();
    h.insert(ACCEPT, "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8".parse().unwrap());
    h.insert(ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8".parse().unwrap());
    h.insert(CACHE_CONTROL, "no-cache".parse().unwrap());
    // Referer 指向工坊浏览页，配合 UA 伪装浏览器会话
    h.insert(REFERER, "https://steamcommunity.com/workshop/".parse().unwrap());
    h.insert("sec-fetch-dest", "document".parse().unwrap());
    h.insert("sec-fetch-mode", "navigate".parse().unwrap());
    h.insert("sec-fetch-site", "same-origin".parse().unwrap());
    h.insert("upgrade-insecure-requests", "1".parse().unwrap());
    h.insert(CONNECTION, "keep-alive".parse().unwrap());
    h
}

#[derive(Clone)]
pub struct SteamClient {
    inner: reqwest::Client,
    /// 当前 sessionid（首次请求时从 steamcommunity.com 抓取）
    session: Arc<Mutex<Option<String>>>,
}

impl SteamClient {
    /// 底层 reqwest 客户端（网络探测等复用）
    pub fn http(&self) -> &reqwest::Client {
        &self.inner
    }

    pub fn new(proxy: Option<String>, follow_system_proxy: bool) -> Result<Self, String> {
        let mut builder = reqwest::Client::builder()
            .user_agent(UA)
            .timeout(Duration::from_secs(30));
        if let Some(p) = proxy.filter(|p| !p.is_empty()) {
            builder = builder
                .proxy(reqwest::Proxy::all(&p).map_err(|e| format!("代理配置无效: {e}"))?);
            tracing::info!("steam client using proxy {p}");
        } else if !follow_system_proxy {
            builder = builder.no_proxy();
            tracing::info!("steam client: system proxy disabled, direct connection");
        } else {
            tracing::info!("steam client: following system proxy");
        }
        Ok(Self {
            inner: builder.build().map_err(|e| format!("HTTP 客户端初始化失败: {e}"))?,
            session: Arc::new(Mutex::new(None)),
        })
    }

    /// 从 steamcommunity.com 抓取 sessionid（浏览器访问时 Set-Cookie）。
    /// 有令牌/已登录账号的社区请求需要携带该值，否则返回 403。
    async fn ensure_session(&self) {
        if let Ok(guard) = self.session.lock() {
            if guard.is_some() {
                return;
            }
        }
        // 先发一次 GET，读 Set-Cookie 里的 sessionid；reqwest cookie_store 也会自动保存
        let req = self
            .inner
            .get(SESSION_URL)
            .headers(browser_headers());
        if let Ok(resp) = req.send().await {
            let cookies: Vec<String> = resp
                .headers()
                .get_all(reqwest::header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok().map(|s| s.to_string()))
                .collect();
            for c in &cookies {
                if let Some(sid) = parse_sessionid_from_set_cookie(c) {
                    if let Ok(mut g) = self.session.lock() {
                        *g = Some(sid.clone());
                        tracing::info!("steam client: acquired sessionid ({} chars)", sid.len());
                    }
                    return;
                }
            }
        }
        tracing::warn!("steam client: sessionid 获取失败，工坊请求可能被 403");
    }

    /// 当前 sessionid（供外部诊断/日志）
    #[allow(dead_code)]
    pub fn session_id(&self) -> Option<String> {
        self.session.lock().ok().and_then(|g| g.clone())
    }

    /// 带 sessionid + 浏览器请求头的 GET
    async fn get_with_session(&self, url: &str) -> Result<Response, String> {
        let mut req = self.inner.get(url).headers(browser_headers());
        if let Ok(guard) = self.session.lock() {
            if let Some(sid) = guard.as_ref() {
                req = req.header(reqwest::header::COOKIE, format!("sessionid={sid}"));
            }
        }
        req.send().await.map_err(|e| e.to_string())
    }

    /// 带 sessionid + 浏览器请求头的 POST 表单
    async fn post_form_with_session(
        &self,
        url: &str,
        fields: &[(&str, &str)],
    ) -> Result<Response, String> {
        let mut req = self
            .inner
            .post(url)
            .headers(browser_headers())
            .form(fields);
        if let Ok(guard) = self.session.lock() {
            if let Some(sid) = guard.as_ref() {
                req = req.header(reqwest::header::COOKIE, format!("sessionid={sid}"));
            }
        }
        req.send().await.map_err(|e| e.to_string())
    }

    /// 重取 sessionid（403 时调用）
    fn reset_session(&self) {
        if let Ok(mut g) = self.session.lock() {
            *g = None;
        }
    }

    /// GET：串行 + 指数退避重试（网络错误 / 5xx / 429 / 403）
    pub async fn get(&self, url: &str) -> Result<Response, String> {
        self.ensure_session().await;
        for attempt in 1..=MAX_ATTEMPTS {
            match self.get_with_session(url).await {
                Ok(r) if r.status().is_success() => return Ok(r),
                Ok(r) if retryable(r.status()) => {
                    backoff(attempt).await;
                }
                Ok(r) if r.status() == StatusCode::FORBIDDEN => {
                    if attempt < MAX_ATTEMPTS {
                        self.reset_session();
                        self.ensure_session().await;
                        tracing::info!("steam client: HTTP 403, re-acquired sessionid (attempt {attempt})");
                        backoff(attempt).await;
                    } else {
                        return Err(format!("Steam 接口返回 HTTP 403（可能被 Steam 限制访问，请检查代理/令牌）"));
                    }
                }
                Ok(r) => return Err(format!("Steam 接口返回 HTTP {}", r.status())),
                Err(e) => {
                    if attempt == MAX_ATTEMPTS {
                        return Err(format!("Steam 网络错误: {e}"));
                    }
                    backoff(attempt).await;
                }
            }
        }
        Err("Steam 请求重试耗尽".into())
    }

    /// POST 表单
    pub async fn post_form(&self, url: &str, fields: &[(&str, &str)]) -> Result<Response, String> {
        self.ensure_session().await;
        for attempt in 1..=MAX_ATTEMPTS {
            match self.post_form_with_session(url, fields).await {
                Ok(r) if r.status().is_success() => return Ok(r),
                Ok(r) if retryable(r.status()) => {
                    backoff(attempt).await;
                }
                Ok(r) if r.status() == StatusCode::FORBIDDEN => {
                    if attempt < MAX_ATTEMPTS {
                        self.reset_session();
                        self.ensure_session().await;
                        tracing::info!("steam client: HTTP 403, re-acquired sessionid (attempt {attempt})");
                        backoff(attempt).await;
                    } else {
                        return Err(format!("Steam 接口返回 HTTP 403（可能被 Steam 限制访问，请检查代理/令牌）"));
                    }
                }
                Ok(r) => return Err(format!("Steam 接口返回 HTTP {}", r.status())),
                Err(e) => {
                    if attempt == MAX_ATTEMPTS {
                        return Err(format!("Steam 网络错误: {e}"));
                    }
                    backoff(attempt).await;
                }
            }
        }
        Err("Steam 请求重试耗尽".into())
    }
}

/// 从单个 Set-Cookie 头解析 sessionid（形如 `sessionid=xxxx; Path=/; ...`）
fn parse_sessionid_from_set_cookie(set_cookie: &str) -> Option<String> {
    set_cookie
        .split(';')
        .next()
        .map(|p| p.trim())
        .filter(|p| p.starts_with("sessionid="))
        .map(|p| p["sessionid=".len()..].to_string())
        .filter(|s| !s.is_empty())
}

fn retryable(s: StatusCode) -> bool {
    s.is_server_error() || s == StatusCode::TOO_MANY_REQUESTS
}

async fn backoff(attempt: u32) {
    tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt))).await;
}
