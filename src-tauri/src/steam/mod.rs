//! Steam 客户端：统一 HTTP 客户端（代理 + 串行重试），对齐 Web 版 net.ts / steamFetch

pub mod browse;
pub mod details;
pub mod types;

use std::time::Duration;
use reqwest::{Response, StatusCode};

pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";
const MAX_ATTEMPTS: u32 = 3;

#[derive(Clone)]
pub struct SteamClient {
    inner: reqwest::Client,
}

impl SteamClient {
    /// 底层 reqwest 客户端（网络探测等复用）
    pub fn http(&self) -> &reqwest::Client {
        &self.inner
    }

    pub fn new(proxy: Option<String>) -> Result<Self, String> {
        let mut builder = reqwest::Client::builder()
            .user_agent(UA)
            .timeout(Duration::from_secs(30));
        if let Some(p) = proxy.filter(|p| !p.is_empty()) {
            builder = builder
                .proxy(reqwest::Proxy::all(&p).map_err(|e| format!("代理配置无效: {e}"))?);
            tracing::info!("steam client using proxy {p}");
        }
        Ok(Self {
            inner: builder.build().map_err(|e| format!("HTTP 客户端初始化失败: {e}"))?,
        })
    }

    /// GET：串行 + 指数退避重试（网络错误 / 5xx / 429）
    pub async fn get(&self, url: &str) -> Result<Response, String> {
        for attempt in 1..=MAX_ATTEMPTS {
            match self.inner.get(url).send().await {
                Ok(r) if r.status().is_success() => return Ok(r),
                Ok(r) if retryable(r.status()) => {
                    backoff(attempt).await;
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
        for attempt in 1..=MAX_ATTEMPTS {
            match self.inner.post(url).form(fields).send().await {
                Ok(r) if r.status().is_success() => return Ok(r),
                Ok(r) if retryable(r.status()) => {
                    backoff(attempt).await;
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

fn retryable(s: StatusCode) -> bool {
    s.is_server_error() || s == StatusCode::TOO_MANY_REQUESTS
}

async fn backoff(attempt: u32) {
    tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt))).await;
}
