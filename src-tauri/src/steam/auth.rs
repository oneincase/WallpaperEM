//! Steam 网页会话（JWT）登录：IAuthenticationService 的四个方法 + 社区 cookie。
//!
//! 用途：拿到 `steamLoginSecure` cookie 后读取「我的订阅」等仅登录可见的页面。
//! 与 steamcmd 下载通道无关（那边走 PTY + 账号密码），这里产出的 refresh token
//! 单独持久化，之后同步订阅免输 Steam Guard。
//!
//! 协议对齐 node-steam-session（已用真实接口逐方法抓包验证，2026-09）：
//!   GetPasswordRSAPublicKey → RSA 加密密码 → BeginAuthSessionViaCredentials
//!   →（可选）UpdateAuthSessionWithSteamGuardCode → PollAuthSessionStatus
//!   → access/refresh token；续期走 login.steampowered.com 的 ajaxrefresh 流程
//!   （WebAPI 的 GenerateAccessTokenForApp 会拒绝 WebBrowser 平台的 refresh token）。
//!
//! 报文格式：application/x-www-form-urlencoded POST + JSON 响应（{"response":{...}}）。
//! 注意不要用 protobuf POST：实测 BeginAuthSessionViaCredentials/QR 能容忍，
//! 但 PollAuthSessionStatus 会直接 400「Missing required routing parameter」；
//! 且无论请求格式如何，响应永远是 JSON（Accept: application/x-protobuf 无效）。
//! form 字段约定：u64 用十进制字符串，bytes 用标准 base64，bool 用 "1"。

use super::{rsa, SteamClient, UA};

const API_BASE: &str = "https://api.steampowered.com/IAuthenticationService";

/// EAuthTokenPlatformType::WebBrowser（产出的 token 才能当社区 cookie 用）
const PLATFORM_WEB_BROWSER: u64 = 2;
const WEBSITE_ID: &str = "Community";

/// EAuthSessionGuardType
pub const GUARD_EMAIL_CODE: i32 = 2;
pub const GUARD_DEVICE_CODE: i32 = 3;
pub const GUARD_DEVICE_CONFIRMATION: i32 = 4;
pub const GUARD_EMAIL_CONFIRMATION: i32 = 5;

#[derive(Debug, Clone)]
pub struct AllowedConfirmation {
    pub confirmation_type: i32,
    pub associated_message: String,
}

#[derive(Debug)]
pub struct BeginResult {
    pub client_id: u64,
    pub request_id: Vec<u8>,
    pub interval_sec: f32,
    pub steamid: u64,
    pub allowed: Vec<AllowedConfirmation>,
}

#[derive(Debug, Default)]
pub struct PollResult {
    pub access_token: String,
    pub refresh_token: String,
    pub account_name: String,
    /// 扫码会话专用：服务端下发的「新二维码」URL（二维码会定时轮换）。
    /// 为空表示沿用上一张；拿到 token 后忽略
    pub new_challenge: String,
    /// 服务端轮换的新 client_id（非 0 时后续轮询必须改用它，
    /// 否则一直轮旧 id 永远等不到结果——验证码提交后常见）
    pub new_client_id: u64,
}

/// 读 x-eresult 响应头（缺省按 OK 处理：WebAPI 正常响应都带 1）
fn eresult_of(resp: &reqwest::Response) -> i64 {
    resp.headers()
        .get("x-eresult")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

/// form-encoded POST，返回 (eresult, JSON 的 response 对象)
async fn call(
    base: &str,
    client: &SteamClient,
    method: &str,
    form: &[(String, String)],
) -> Result<(i64, serde_json::Value), String> {
    let url = format!("{base}/{method}/v1/");
    let resp = client
        .http()
        .post(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .form(form)
        .send()
        .await
        .map_err(|e| format!("{method} 请求失败: {e}"))?;
    let eresult = eresult_of(&resp);
    let text = resp
        .text()
        .await
        .map_err(|e| format!("{method} 读取响应失败: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
        format!(
            "{method} 响应不是 JSON（eresult={eresult}）：{}",
            &text[..text.len().min(120)]
        )
    })?;
    let r = v.get("response").cloned().unwrap_or(serde_json::Value::Null);
    Ok((eresult, r))
}

/// JSON 里的 u64：Steam 把 64 位整数序列化成十进制字符串，但也兼容数字字面量
fn json_u64(v: &serde_json::Value, key: &str) -> u64 {
    match v.get(key) {
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0),
        Some(x) => x.as_u64().unwrap_or(0),
        None => 0,
    }
}

/// JSON 里的字符串字段（缺失返回空串）
fn json_str(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// JSON 里的 base64 bytes 字段
fn json_b64(v: &serde_json::Value, key: &str) -> Vec<u8> {
    base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        json_str(v, key),
    )
    .unwrap_or_default()
}

/// EResult → 用户可读描述。注意 5（InvalidPassword）的真实含义以 Steam 为准：
/// 它只说明「这次账号+密码没被接受」，常见于本地存的密码已过期 ——
/// steamcmd 下载走它自己缓存的登录态，密码错了照样能下载，不能反推密码有效
fn eresult_msg(method: &str, eresult: i64) -> String {
    let desc = match eresult {
        5 => "账号名或密码错误",
        6 => "账号已在别处登录",
        8 => "请求参数无效",
        84 => "请求过于频繁，请稍后再试",
        88 => "验证码错误",
        89 => "登录会话已过期，请重新开始",
        _ => "Steam 返回错误",
    };
    format!("{method} 失败（EResult {eresult}）：{desc}")
}

/// 是否为「密码不被接受」（前端据此给出重新输入凭据的表单）
pub fn is_invalid_password(err: &str) -> bool {
    err.contains("EResult 5")
}

/// GetPasswordRSAPublicKey（JSON，不需要密钥）
async fn password_rsa_key(
    base: &str,
    client: &SteamClient,
    account: &str,
) -> Result<(String, String, u64), String> {
    let url = format!(
        "{base}/GetPasswordRSAPublicKey/v1/?account_name={}",
        crate::util::url_encode(account)
    );
    let resp = client
        .http()
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("获取 RSA 公钥失败: {e}"))?;
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("RSA 公钥响应解析失败: {e}"))?;
    let r = v.get("response").cloned().unwrap_or_default();
    let m = r
        .get("publickey_mod")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let e = r
        .get("publickey_exp")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let ts = r
        .get("timestamp")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if m.is_empty() || e.is_empty() || ts == 0 {
        return Err("RSA 公钥响应缺少字段（账号不存在？）".into());
    }
    Ok((m, e, ts))
}

/// 用账号密码开启登录会话
pub async fn begin_with_password(
    client: &SteamClient,
    account: &str,
    password: &str,
) -> Result<BeginResult, String> {
    begin_with_password_at(API_BASE, client, account, password).await
}

/// 可注入服务器地址的版本（端到端测试指向本地 mock 用）
pub async fn begin_with_password_at(
    base: &str,
    client: &SteamClient,
    account: &str,
    password: &str,
) -> Result<BeginResult, String> {
    let (m, e, ts) = password_rsa_key(base, client, account).await?;
    let encrypted = rsa::encrypt_pkcs1v15(&m, &e, password.as_bytes(), &rsa::random_pad)?;
    let enc_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        encrypted,
    );

    let form = [
        ("account_name".to_string(), account.to_string()),
        ("encrypted_password".to_string(), enc_b64),
        ("encryption_timestamp".to_string(), ts.to_string()),
        ("remember_login".to_string(), "1".to_string()),
        ("platform_type".to_string(), PLATFORM_WEB_BROWSER.to_string()),
        ("website_id".to_string(), WEBSITE_ID.to_string()),
        (
            "device_friendly_name".to_string(),
            "WallpaperEM (Community)".to_string(),
        ),
    ];
    let (eresult, r) = call(base, client, "BeginAuthSessionViaCredentials", &form).await?;
    if eresult != 1 {
        return Err(eresult_msg(crate::i18n::tr("登录"), eresult));
    }
    let mut out = BeginResult {
        client_id: json_u64(&r, "client_id"),
        request_id: json_b64(&r, "request_id"),
        interval_sec: r
            .get("interval")
            .and_then(|x| x.as_f64())
            .unwrap_or(5.0) as f32,
        steamid: json_u64(&r, "steamid"),
        allowed: Vec::new(),
    };
    if let Some(confs) = r.get("allowed_confirmations").and_then(|x| x.as_array()) {
        for c in confs {
            out.allowed.push(AllowedConfirmation {
                confirmation_type: json_u64(c, "confirmation_type") as i32,
                associated_message: json_str(c, "associated_message"),
            });
        }
    }
    if out.client_id == 0 || out.request_id.is_empty() || out.steamid == 0 {
        return Err(format!(
            "登录响应缺少会话字段（eresult=1 但 client_id/request_id/steamid 不完整）：{r}"
        ));
    }
    Ok(out)
}

/// 扫码登录会话开启结果。
/// 注意：QR 的 Begin 响应没有 steamid（密码登录有）——扫码的 steamid 要等
/// 轮询拿到 access_token 后从 JWT 的 sub 字段解出（见 steamid_from_jwt）
#[derive(Debug)]
pub struct BeginQrResult {
    pub client_id: u64,
    pub request_id: Vec<u8>,
    pub interval_sec: f32,
    /// 要渲染成二维码的 URL（形如 https://s.team/q/1/...，直接作为二维码内容）
    pub challenge_url: String,
}

/// 开启扫码登录会话（无需账号密码；用户用 Steam 手机 App 扫码确认）
pub async fn begin_qr(client: &SteamClient) -> Result<BeginQrResult, String> {
    begin_qr_at(API_BASE, client).await
}

/// 可注入服务器地址的版本（端到端测试指向本地 mock 用）
pub async fn begin_qr_at(base: &str, client: &SteamClient) -> Result<BeginQrResult, String> {
    let form = [
        (
            "device_friendly_name".to_string(),
            "WallpaperEM (Community)".to_string(),
        ),
        ("platform_type".to_string(), PLATFORM_WEB_BROWSER.to_string()),
        ("website_id".to_string(), WEBSITE_ID.to_string()),
    ];
    let (eresult, r) = call(base, client, "BeginAuthSessionViaQR", &form).await?;
    if eresult != 1 {
        return Err(eresult_msg(crate::i18n::tr("扫码登录"), eresult));
    }
    let out = BeginQrResult {
        client_id: json_u64(&r, "client_id"),
        request_id: json_b64(&r, "request_id"),
        interval_sec: r
            .get("interval")
            .and_then(|x| x.as_f64())
            .unwrap_or(5.0) as f32,
        challenge_url: json_str(&r, "challenge_url"),
    };
    if out.client_id == 0 || out.request_id.is_empty() || out.challenge_url.is_empty() {
        return Err(format!(
            "扫码登录响应缺少会话字段（eresult=1 但 client_id/request_id/challenge_url 不完整）：{r}"
        ));
    }
    Ok(out)
}

/// 提交 Steam Guard 验证码
pub async fn submit_guard_code(
    client: &SteamClient,
    client_id: u64,
    steamid: u64,
    code: &str,
    code_type: i32,
) -> Result<(), String> {
    let form = [
        ("client_id".to_string(), client_id.to_string()),
        ("steamid".to_string(), steamid.to_string()),
        ("code".to_string(), code.to_string()),
        ("code_type".to_string(), code_type.to_string()),
    ];
    let (eresult, _) = call(API_BASE, client, "UpdateAuthSessionWithSteamGuardCode", &form).await?;
    // 1 = 成功；6（DuplicateRequest）= 码已提交过，按成功继续轮询
    if eresult != 1 && eresult != 6 {
        return Err(eresult_msg(crate::i18n::tr("验证码提交"), eresult));
    }
    Ok(())
}

/// 轮询登录结果（拿到 token 前响应体为空；eresult 85/89 表示继续等）
pub async fn poll_status(
    client: &SteamClient,
    client_id: u64,
    request_id: &[u8],
) -> Result<Option<PollResult>, String> {
    poll_status_at(API_BASE, client, client_id, request_id).await
}

/// 可注入服务器地址的版本（测试指向本地 mock 用）
pub async fn poll_status_at(
    base: &str,
    client: &SteamClient,
    client_id: u64,
    request_id: &[u8],
) -> Result<Option<PollResult>, String> {
    let form = [
        ("client_id".to_string(), client_id.to_string()),
        (
            "request_id".to_string(),
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, request_id),
        ),
    ];
    let (eresult, r) = call(base, client, "PollAuthSessionStatus", &form).await?;
    if eresult != 1 {
        // 85 Timeout / 89 Expired：还没确认完，继续等
        if eresult == 85 || eresult == 89 {
            return Ok(None);
        }
        return Err(eresult_msg(crate::i18n::tr("轮询登录状态"), eresult));
    }
    // 等待中时响应形如 {"had_remote_interaction": false}（eresult=1 但无 token）
    let out = PollResult {
        new_challenge: json_str(&r, "new_challenge"),
        refresh_token: json_str(&r, "refresh_token"),
        access_token: json_str(&r, "access_token"),
        account_name: json_str(&r, "account_name"),
        new_client_id: json_u64(&r, "new_client_id"),
    };
    // 拿到 token 算成功；扫码场景下只拿到轮换的新二维码也要透传给前端
    if (out.access_token.is_empty() || out.refresh_token.is_empty())
        && out.new_challenge.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(out))
}

/// Web 会话续期结果
#[derive(Debug)]
pub struct WebSessionTokens {
    pub access_token: String,
    /// rtExpiry：本次登录授权的到期时间（秒级时间戳），仅用于日志展示
    pub rt_expiry: u64,
}

/// 网页会话续期（与 Steam 官方 auth_refresh.js 完全同款的 ajaxrefresh 流程）：
///   1) POST login.steampowered.com/jwt/ajaxrefresh（带 steamLoginSecure cookie + redir）
///   2) 把响应字段原样 + prior=旧 access token POST 到响应指定的 login_url
///   3) 拿回新 access token，写回本地会话
///
/// 为什么不走 IAuthenticationService/GenerateAccessTokenForApp：
/// 它的 WebAPI 网关会拒绝 WebBrowser 平台签发的 refresh token（实测新鲜 token
/// 也是 EResult 15 AccessDenied）——那条路只对 CM 协议/客户端平台有效。
/// 网页会话续期 Steam 官方网页就是走这里的 ajaxrefresh。
/// access token 过期也能续（服务器按内嵌的 rt_exp 授权期校验，约 200 天）。
pub async fn renew_web_session(
    client: &SteamClient,
    steamid: u64,
    access_token: &str,
) -> Result<WebSessionTokens, String> {
    renew_web_session_at("https://login.steampowered.com", client, steamid, access_token).await
}

/// 可注入服务器地址的版本（测试指向本地 mock 用）
pub async fn renew_web_session_at(
    base: &str,
    client: &SteamClient,
    steamid: u64,
    access_token: &str,
) -> Result<WebSessionTokens, String> {
    // sessionid 优先用服务端签发的（随机自造的可能被 ajaxrefresh 拒）：
    // 先 GET 一次登录域拿 Set-Cookie，拿不到再退回随机
    let sessionid = harvest_sessionid(client, base).await;
    let cookie = community_cookie(steamid, access_token, &sessionid);
    // 诊断：记录被续期 token 的关键声明（aud 必须含 "web" 才能当网页 cookie 用）
    if let Some(claims) = jwt_claims(access_token) {
        tracing::info!(
            "订阅同步：准备续期 token sub={} aud={} exp={}",
            claims.get("sub").cloned().unwrap_or_default(),
            claims.get("aud").cloned().unwrap_or_default(),
            claims.get("exp").cloned().unwrap_or_default(),
        );
    }

    // 1) ajaxrefresh：用当前 cookie 换一个 finalize 地址 + nonce
    let resp = client
        .http()
        .post(format!("{base}/jwt/ajaxrefresh"))
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::COOKIE, &cookie)
        .header(reqwest::header::ORIGIN, "https://steamcommunity.com")
        .header(reqwest::header::REFERER, "https://steamcommunity.com/")
        .form(&[("redir", "https://steamcommunity.com/")])
        .send()
        .await
        .map_err(|e| format!("网页会话续期请求失败: {e}"))?;
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("网页会话续期响应解析失败: {e}"))?;
    if v.get("success").and_then(|x| x.as_bool()) != Some(true) {
        let code = json_u64(&v, "error");
        return Err(format!(
            "网页会话续期被服务器拒绝（error={code}，完整响应：{v}），需要重新登录"
        ));
    }
    let login_url = json_str(&v, "login_url");
    if login_url.is_empty() {
        return Err("网页会话续期：ajaxrefresh 响应缺少 login_url".into());
    }

    // 2) finalizelogin：ajaxrefresh 响应字段全量回传 + prior=旧 access token
    //    （官方 JS 就是 Object.assign(response, {prior: ...}) 原样 POST 回去）
    let mut form: Vec<(String, String)> = Vec::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            match val {
                serde_json::Value::String(s) => form.push((k.clone(), s.clone())),
                serde_json::Value::Number(_) | serde_json::Value::Bool(_) => {
                    form.push((k.clone(), val.to_string()))
                }
                _ => {}
            }
        }
    }
    form.push(("prior".to_string(), access_token.to_string()));
    let resp = client
        .http()
        .post(&login_url)
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::COOKIE, &cookie)
        .header(reqwest::header::ORIGIN, "https://steamcommunity.com")
        .header(reqwest::header::REFERER, "https://steamcommunity.com/")
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("网页会话续期 finalize 请求失败: {e}"))?;
    let v2: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("网页会话续期 finalize 响应解析失败: {e}"))?;
    let result = v2.get("result").and_then(|x| x.as_i64()).unwrap_or(0);
    let token = json_str(&v2, "token");
    if result != 1 || token.is_empty() {
        return Err(format!(
            "网页会话续期失败（result={result}），需要重新登录"
        ));
    }
    Ok(WebSessionTokens {
        access_token: token,
        rt_expiry: json_u64(&v2, "rtExpiry"),
    })
}

/// 解出 JWT 的 payload（不验签——我们只是读取自己刚拿到的 token 的声明）
pub fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        payload,
    )
    .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 从 access_token (JWT) 解出 steamid：payload 的 sub 字段。
/// 扫码登录的 Begin 响应不带 steamid，只能等拿到 token 后从这里解
pub fn steamid_from_jwt(token: &str) -> Option<u64> {
    let sub = jwt_claims(token)?.get("sub")?.clone();
    sub.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| sub.as_u64())
}

/// JWT 的过期时间（exp，秒级时间戳）
pub fn jwt_exp(token: &str) -> Option<u64> {
    jwt_claims(token)?.get("exp")?.as_u64()
}

/// 拼社区 cookie 头：steamLoginSecure=<steamid>||<jwt>（|| 需百分号编码）
pub fn community_cookie(steamid: u64, access_token: &str, sessionid: &str) -> String {
    format!("sessionid={sessionid}; steamLoginSecure={steamid}%7C%7C{access_token}")
}

/// GET 一次目标站点，从 Set-Cookie 收割服务端签发的 sessionid；失败返回 None
async fn harvest_sessionid(client: &SteamClient, base: &str) -> String {
    let issued = client
        .http()
        .get(base)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .ok()
        .and_then(|resp| {
            resp.headers()
                .get_all(reqwest::header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .find_map(|s| {
                    s.split(';')
                        .next()
                        .and_then(|p| p.trim().strip_prefix("sessionid="))
                        .filter(|sid| !sid.is_empty())
                        .map(|sid| sid.to_string())
                })
        });
    match issued {
        Some(sid) => sid,
        None => new_sessionid(),
    }
}

/// 随机 sessionid（24 位 hex，与 Steam 签发格式一致即可，服务端不校验来源）
pub fn new_sessionid() -> String {
    (0..12)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect()
}

/// 抓取登录账号的一页工坊订阅（每页 30 条），返回 (本页 id 列表, 订阅总数)。
///
/// 数据源：steamcommunity.com/profiles/<steamid>/myworkshopfiles/?browsefilter=mysubscriptions
/// ——与浏览器里「您的工坊文件 → 订阅的物品」完全相同，服务端直出条目链接
/// （2026-09 实测：21 个订阅读出 63 个锚点，每条约 3 个）。
/// 注意 /my/myworkshopfiles/?section=subscriptions 不能用：该页已改成响应式外壳，
/// 订阅列表不在原始 HTML 里；workshop/browse?browsesort=mysubscriptions 也不可靠。
/// 总数从分页信息 div 解析（本地化文案「正在显示第 1 - 30 项，共 288 项条目」/
/// "Showing 1-30 of 288"，取 div 内最大数字，语言无关）。
///
/// dump_dir：page=1 一条都没读到时把页面快照写进去（subscriptions-debug.html），
/// 方便定位是「真的没订阅」还是「页面结构又变了」
pub async fn fetch_subscribed_page(
    client: &SteamClient,
    steamid: u64,
    access_token: &str,
    page: u32,
    dump_dir: Option<&std::path::Path>,
) -> Result<(Vec<String>, u64), String> {
    fetch_subscribed_page_at(
        "https://steamcommunity.com",
        client,
        steamid,
        access_token,
        page,
        dump_dir,
    )
    .await
}

/// 可注入站点根地址的版本（测试指向本地 mock 用）
async fn fetch_subscribed_page_at(
    base: &str,
    client: &SteamClient,
    steamid: u64,
    access_token: &str,
    page: u32,
    dump_dir: Option<&std::path::Path>,
) -> Result<(Vec<String>, u64), String> {
    let sessionid = new_sessionid();
    let cookie = community_cookie(steamid, access_token, &sessionid);
    let id_re = regex::Regex::new(r"sharedfiles/filedetails/\?id=(\d+)").unwrap();
    // 两种总数来源：browse 页的内嵌 JSON（引号前有 1~2 个反斜杠）+
    // myworkshopfiles 页的分页信息 div（本地化文案，取最大数字）
    let total_re = regex::Regex::new(r#"total_count\\*":(\d+)"#).unwrap();
    let paging_re = regex::Regex::new(r#"(?s)workshopBrowsePagingInfo[^>]*>(.*?)</div>"#).unwrap();
    let num_re = regex::Regex::new(r"[\d,]+").unwrap();

    let url = format!(
        "{base}/profiles/{steamid}/myworkshopfiles/?appid=431960&browsefilter=mysubscriptions&p={page}&numperpage=30"
    );
    let resp = client
        .http()
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::COOKIE, &cookie)
        .header(
            reqwest::header::ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header(
            reqwest::header::REFERER,
            format!("https://steamcommunity.com/profiles/{steamid}/myworkshopfiles/"),
        )
        .send()
        .await
        .map_err(|e| format!("读取订阅列表失败: {e}"))?;
    let final_url = resp.url().to_string();
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("读取订阅列表失败: {e}"))?;
    tracing::info!(
        "订阅同步：抓取第 {page} 页，HTTP {status}，{} 字节，最终 URL {final_url}",
        body.len()
    );
    // 未登录/登录失效：重定向到登录页，或页面标记 g_steamID = false
    if final_url.contains("/login") || body.contains("g_steamID = false") {
        return Err("登录态已失效（订阅页判定为未登录），请重新扫码或验证账号".into());
    }
    // 总数：JSON total_count 与分页信息 div 取较大者（两种页面结构都兼容）。
    // 分页 div 内数字可能带千分位逗号（2,885,344），先去掉再 parse。
    let json_total: u64 = total_re
        .captures(&body)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0);
    let paging_total: u64 = paging_re
        .captures(&body)
        .map(|c| {
            num_re
                .find_iter(&c[1])
                .filter_map(|m| m.as_str().replace(',', "").parse::<u64>().ok())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let total = json_total.max(paging_total);
    let mut ids: Vec<String> = Vec::new();
    for m in id_re.find_iter(&body) {
        let id = m.as_str().trim_start_matches("sharedfiles/filedetails/?id=");
        if !ids.iter().any(|x| x == id) {
            ids.push(id.to_string());
        }
    }

    if ids.is_empty() && page == 1 {
        // 区分「真的没有订阅」与「页面结构变了」：把页面快照落盘方便排查
        let mut hint = String::new();
        if let Some(dir) = dump_dir {
            let _ = std::fs::create_dir_all(dir);
            let path = dir.join("subscriptions-debug.html");
            if std::fs::write(&path, &body).is_ok() {
                hint = format!("。页面快照已保存到 {}，请反馈", path.display());
            }
        }
        return Err(format!(
            "没有读到任何订阅条目（HTTP {status}，页面 {} 字节）。\
             若账号确实有订阅，可能是订阅页结构已变化{hint}",
            body.len()
        ));
    }
    Ok((ids, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 实测：用本地保存的登录凭证跑生产抓取函数（真实 Steam），
    /// 校验条目解析与总数。手动跑：
    /// cargo test --offline --lib -- --ignored real_subs_page --nocapture
    #[tokio::test]
    #[ignore]
    async fn real_subs_page_probe() {
        let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
            .join("Library/Application Support/io.github.oneincase.wallpaperem");
        let Some((_username, steamid, token)) = crate::secure_store::load_session_any(&dir)
        else {
            panic!("没有已保存的登录会话");
        };
        let client = crate::steam::SteamClient::new(None, false).unwrap();
        let (ids, total) = fetch_subscribed_page(&client, steamid, &token, 1, None)
            .await
            .expect("真实抓取应成功");
        println!("total={total}, page1 ids={} -> {:?}", ids.len(), ids);
        assert!(!ids.is_empty(), "该账号应有订阅");
        assert!(total >= ids.len() as u64, "总数不应小于本页条数");
    }


    /// 端到端互验：本地 mock Steam 服务器 + 独立实现的 RSA 解密 / PKCS#1 拆包，
    /// 完整验证 form 编码请求（字段取值、RSA 加密密码）与 JSON 响应解析。
    /// 私钥只存在于本测试，不与任何真实账号相关。
    const TEST_MODULUS_HEX: &str = "d6d888d3a368d7cf9767d554fd8a49e567240d34fe352eb663b3d7dfa3be8df85b4f6bd6ae861bdaf6e06a3e2ffeb54f141d4051d81620b9eb7fc05f00a961783e3773e80914ceaf4fcd5858800523ff4569217c1e067e69d3489c198d368fdf396ad6425788691edb8b4958370aef91acc3def4e1b62f02dae3386100f692d7fedad5650c28b9f77a9f9054df705f2727867e487ebefa34d146d462687ecd9ab360e47aadc18133764f941e5e7ebb9fd1da2145112dc8718a9414afce8594a6bc62f58a9aae98f32e8fbb0298326ea36fd1be4d4aac6c48dfd075af6673c1fcd44245c143c5f2c3088eb8bf5c36c9d5ee0e5a06ab4f5982f83a9013b60aadbf";
    const TEST_PRIVATE_EXP_HEX: &str = "2eb04bbbc24d2c68fe7c200e223305300723fc82c1a3890d35c98566224d6cc8c5ff126e4aeaf5eeb5abbb2adc7f3ba37db9859ac39cbb6bebd38d5897ea37364c3efcbf360a0188738d2a5fc1225cda4299401f9adeca65f0f65c85e8fc2c73d424757f614a519dd51405d257d3d6900fbd591c5a589f0abdca971bed7ba819462dab3a4c1d5d9435bdaf3115b9e3073d21653c449226702c5ee90cbdf472efde4f19dfe22024f4d8684becb4abd6250959fa8ec83dc3d6e5aaa4227772e16a0e27de8e67f614875dda273daa3015f2c576db678a289c7c612c4242d4f6c6cefaa42f9ee574e1510f4aad3e7c226c4d17816a5a9268499127ffa5502cd9a4d1";

    /// RSA 解密（c^d mod n）+ PKCS#1 v1.5 拆包，返回明文
    fn rsa_decrypt_pkcs1(ct: &[u8]) -> Option<Vec<u8>> {
        let m = hex_bytes(TEST_MODULUS_HEX);
        let d = hex_bytes(TEST_PRIVATE_EXP_HEX);
        let modulus = super::super::rsa::from_be_bytes_pub(&m);
        let c = super::super::rsa::from_be_bytes_pub(ct);
        let plain = super::super::rsa::modpow_pub(&c, &d, &modulus);
        let em = super::super::rsa::to_be_bytes_pub(&plain, m.len());
        // EM = 00 02 PS(>=8 非零) 00 M
        if em.len() != m.len() || em[0] != 0 || em[1] != 2 {
            return None;
        }
        let sep = em[2..].iter().position(|&b| b == 0).map(|i| i + 2)?;
        if sep < 2 + 8 {
            return None; // PS 不足 8 字节，不是合法 v1.5 块
        }
        Some(em[sep + 1..].to_vec())
    }

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// form-urlencoded 请求体 → HashMap
    fn parse_form(body: &[u8]) -> std::collections::HashMap<String, String> {
        url::form_urlencoded::parse(body).into_owned().collect()
    }

    /// 起一个带 x-eresult=1 头的 JSON mock 服务器
    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/IAuthenticationService")
    }

    fn eresult_header() -> [(axum::http::header::HeaderName, &'static str); 1] {
        [(axum::http::header::HeaderName::from_static("x-eresult"), "1")]
    }

    /// 造一页 myworkshopfiles HTML：n 个条目（每条 3 个锚点，与真实页面一致）
    /// + 中文分页信息 div（真实页面总数在这里，无 total_count JSON）
    fn browse_page_html(start_id: u64, n: usize, total: u64) -> String {
        let mut items = String::new();
        for i in 0..n {
            let id = start_id + i as u64;
            items.push_str(&format!(
                r#"<a href="https://steamcommunity.com/sharedfiles/filedetails/?id={id}"><img src="x{id}.jpg"></a>
<a href="https://steamcommunity.com/sharedfiles/filedetails/?id={id}">t{id}</a>
<a href="https://steamcommunity.com/sharedfiles/filedetails/?id={id}">c{id}</a>"#
            ));
        }
        format!(
            r#"<html><script>g_steamID = "76561198000000001";</script><body>{items}
<div class="workshopBrowsePagingInfo">正在显示第 1 - {n} 项，共 {total} 项条目</div></body></html>"#
        )
    }

    #[tokio::test]
    async fn fetch_subscribed_page_reads_ids_total_and_dedupes() {
        let app = axum::Router::new().route(
            "/profiles/{steamid}/myworkshopfiles/",
            axum::routing::get(|q: axum::extract::Query<
                std::collections::HashMap<String, String>,
            >| async move {
                assert_eq!(q["browsefilter"], "mysubscriptions");
                assert_eq!(q["appid"], "431960");
                let page: u64 = q["p"].parse().unwrap();
                let html = match page {
                    2 => browse_page_html(1030, 3, 33), // 第 2 页 3 条
                    _ => browse_page_html(1000, 30, 33), // 第 1 页 30 条（含内部重复锚点）
                };
                axum::response::Html(html)
            }),
        );
        let base = serve(app).await.replace("/IAuthenticationService", "");
        let client = SteamClient::new(None, false).unwrap();

        let (ids, total) = fetch_subscribed_page_at(&base, &client, 1, "token", 1, None)
            .await
            .expect("应抓到第 1 页");
        assert_eq!(ids.len(), 30, "每条 3 个锚点应去重为 30 个 id");
        assert_eq!(ids[0], "1000");
        assert_eq!(total, 33, "总数应从分页信息 div 解析");

        let (ids2, total2) = fetch_subscribed_page_at(&base, &client, 1, "token", 2, None)
            .await
            .expect("应抓到第 2 页");
        assert_eq!(ids2.len(), 3);
        assert_eq!(total2, 33);
    }

    #[tokio::test]
    async fn fetch_subscribed_page_detects_logged_out() {
        let app = axum::Router::new().route(
            "/profiles/{steamid}/myworkshopfiles/",
            axum::routing::get(|| async {
                axum::response::Html(r#"<script>g_steamID = false;</script>"#)
            }),
        );
        let base = serve(app).await.replace("/IAuthenticationService", "");
        let client = SteamClient::new(None, false).unwrap();
        let err = fetch_subscribed_page_at(&base, &client, 1, "token", 1, None)
            .await
            .unwrap_err();
        assert!(err.contains("登录态已失效"), "应识别未登录: {err}");
    }

    #[tokio::test]
    async fn fetch_subscribed_page_dumps_snapshot_when_empty() {
        let app = axum::Router::new().route(
            "/profiles/{steamid}/myworkshopfiles/",
            axum::routing::get(|| async {
                axum::response::Html(r#"<script>g_steamID = "1";</script><p>empty</p>"#)
            }),
        );
        let base = serve(app).await.replace("/IAuthenticationService", "");
        let dir = std::env::temp_dir().join(format!("wem-sub-{}", rand::random::<u32>()));
        let client = SteamClient::new(None, false).unwrap();
        let err = fetch_subscribed_page_at(&base, &client, 1, "token", 1, Some(&dir))
            .await
            .unwrap_err();
        assert!(err.contains("没有读到任何订阅条目"), "{err}");
        assert!(err.contains("页面快照已保存"), "错误里应带快照路径: {err}");
        let dumped = std::fs::read_to_string(dir.join("subscriptions-debug.html")).unwrap();
        assert!(dumped.contains("empty"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn renew_web_session_follows_ajaxrefresh_flow() {
        // 官方 auth_refresh.js 同款两步流：ajaxrefresh 拿 login_url+nonce →
        // 响应字段全量回传 + prior=旧 token → 拿回新 token。
        // login_url 用请求的 Host 头生成，指回 mock 自身
        let app = axum::Router::new()
            // 续期前会先 GET 一次站点根路径收割服务端签发的 sessionid
            .route(
                "/",
                axum::routing::get(|| async {
                    (
                        [(
                            axum::http::header::SET_COOKIE,
                            "sessionid=server-issued-sid; Path=/",
                        )],
                        "ok",
                    )
                }),
            )
            .route(
                "/jwt/ajaxrefresh",
                axum::routing::post(
                    |headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                        // cookie 必须带 steamLoginSecure=<steamid>%7C%7C<旧 token>
                        let cookie = headers
                            .get(axum::http::header::COOKIE)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        assert!(
                            cookie.contains(
                                "steamLoginSecure=76561198000000001%7C%7Cold-access-token"
                            ),
                            "cookie 必须带旧 access token: {cookie}"
                        );
                        assert!(
                            cookie.contains("sessionid=server-issued-sid"),
                            "应优先用服务端签发的 sessionid: {cookie}"
                        );
                        let host = headers
                            .get(axum::http::header::HOST)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let f: std::collections::HashMap<String, String> =
                            url::form_urlencoded::parse(&body).into_owned().collect();
                        assert!(f.contains_key("redir"));
                        axum::Json(serde_json::json!({
                            "success": true,
                            "login_url": format!("http://{host}/jwt/finalizelogin"),
                            "steamid": "76561198000000001",
                            "nonce": "abc123nonce",
                            "redir": "https://steamcommunity.com/",
                        }))
                    },
                ),
            )
            .route(
                "/jwt/finalizelogin",
                axum::routing::post(|body: axum::body::Bytes| async move {
                    let f: std::collections::HashMap<String, String> =
                        url::form_urlencoded::parse(&body).into_owned().collect();
                    // ajaxrefresh 响应字段全量回传 + prior=旧 access token
                    assert_eq!(f["nonce"], "abc123nonce", "ajaxrefresh 的字段要原样回传");
                    assert_eq!(f["steamid"], "76561198000000001");
                    assert_eq!(f["prior"], "old-access-token", "prior 必须是旧 access token");
                    axum::Json(serde_json::json!({
                        "result": 1,
                        "token": "new-access-token",
                        "rtExpiry": 1893456000u64,
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{addr}");
        let client = SteamClient::new(None, false).unwrap();
        let res = renew_web_session_at(&base, &client, 76561198000000001, "old-access-token")
            .await
            .expect("续期应成功");
        assert_eq!(res.access_token, "new-access-token");
        assert_eq!(res.rt_expiry, 1893456000);
    }

    #[tokio::test]
    async fn renew_web_session_rejected_when_session_invalid() {
        // cookie 被拒：success=false + error 码 → 明确报错（调用方据此清理会话）
        let app = axum::Router::new().route(
            "/jwt/ajaxrefresh",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({ "success": false, "error": 21 }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{addr}");
        let client = SteamClient::new(None, false).unwrap();
        let err = renew_web_session_at(&base, &client, 1, "bad-token")
            .await
            .unwrap_err();
        assert!(err.contains("error=21"), "{err}");
        assert!(err.contains("重新登录"), "{err}");
    }

    #[tokio::test]
    async fn begin_auth_end_to_end_against_mock_steam() {
        const TS: u64 = 1_700_000_123;
        const PASSWORD: &str = "correct horse battery 中文!@#";

        // mock：校验 form 字段、RSA 解密出的密码，然后回 JSON 响应
        let app = axum::Router::new()
            .route(
                "/IAuthenticationService/GetPasswordRSAPublicKey/v1/",
                axum::routing::get(|| async {
                    axum::Json(serde_json::json!({
                        "response": {
                            "publickey_mod": TEST_MODULUS_HEX,
                            "publickey_exp": "010001",
                            "timestamp": TS.to_string(),
                        }
                    }))
                }),
            )
            .route(
                "/IAuthenticationService/BeginAuthSessionViaCredentials/v1/",
                axum::routing::post(|body: axum::body::Bytes| async move {
                    let f = parse_form(&body);
                    assert_eq!(f["account_name"], "testaccount");
                    assert_eq!(
                        f["encryption_timestamp"],
                        TS.to_string(),
                        "encryption_timestamp 必须回传公钥响应里的时间戳"
                    );
                    assert_eq!(f["remember_login"], "1");
                    assert_eq!(f["platform_type"], "2", "platform_type = WebBrowser");
                    assert_eq!(f["website_id"], "Community");
                    let ct = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        &f["encrypted_password"],
                    )
                    .expect("encrypted_password 必须是标准 base64");
                    assert_eq!(ct.len(), 256, "密文长度 = 模数字节数");
                    let plain = rsa_decrypt_pkcs1(&ct).expect("PKCS#1 v1.5 拆包失败");
                    assert_eq!(
                        String::from_utf8(plain).unwrap(),
                        PASSWORD,
                        "RSA 解密出的密码必须原样还原"
                    );
                    // JSON 响应：64 位整数按 Steam 惯例序列化成十进制字符串，
                    // request_id 是 base64
                    (
                        eresult_header(),
                        axum::Json(serde_json::json!({
                            "response": {
                                "client_id": "3735928559",
                                "request_id": base64::Engine::encode(
                                    &base64::engine::general_purpose::STANDARD,
                                    [1u8, 2, 3, 4],
                                ),
                                "interval": 5,
                                "steamid": "76561198000000000",
                                "allowed_confirmations": [
                                    { "confirmation_type": 3, "associated_message": "" }
                                ],
                            }
                        })),
                    )
                }),
            );

        let base = serve(app).await;
        let client = SteamClient::new(None, false).unwrap();
        let res = begin_with_password_at(&base, &client, "testaccount", PASSWORD)
            .await
            .expect("mock 登录应成功");
        assert_eq!(res.client_id, 3735928559);
        assert_eq!(res.request_id, vec![1, 2, 3, 4]);
        assert_eq!(res.steamid, 76561198000000000);
        assert!((res.interval_sec - 5.0).abs() < 0.01);
        assert_eq!(res.allowed.len(), 1);
        assert_eq!(res.allowed[0].confirmation_type, 3);
    }

    #[tokio::test]
    async fn begin_qr_end_to_end_against_mock_steam() {
        const CHALLENGE: &str = "https://s.team/q/1/14990133670412958896";

        // mock：校验 form 字段后回与真实接口一致的 JSON
        let app = axum::Router::new().route(
            "/IAuthenticationService/BeginAuthSessionViaQR/v1/",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let f = parse_form(&body);
                assert!(!f["device_friendly_name"].is_empty());
                assert_eq!(f["platform_type"], "2", "platform_type = WebBrowser");
                assert_eq!(f["website_id"], "Community");
                (
                    eresult_header(),
                    axum::Json(serde_json::json!({
                        "response": {
                            "client_id": "14990133670412958896",
                            "challenge_url": CHALLENGE,
                            "request_id": base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                [9u8, 8, 7],
                            ),
                            "interval": 5,
                            "allowed_confirmations": [
                                { "confirmation_type": 4 },
                                { "confirmation_type": 3 }
                            ],
                            "version": 1,
                        }
                    })),
                )
            }),
        );
        let base = serve(app).await;
        let client = SteamClient::new(None, false).unwrap();
        let res = begin_qr_at(&base, &client)
            .await
            .expect("mock 扫码会话应成功");
        assert_eq!(res.client_id, 14990133670412958896);
        assert_eq!(res.request_id, vec![9, 8, 7]);
        assert_eq!(res.challenge_url, CHALLENGE);
        assert!((res.interval_sec - 5.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn poll_status_surfaces_rotated_challenge() {
        // 服务端只下发轮换的新二维码（无 token）时必须透传给前端，
        // 不能被当成「继续等」丢掉——否则二维码过期用户扫不上
        let app = axum::Router::new().route(
            "/IAuthenticationService/PollAuthSessionStatus/v1/",
            axum::routing::post(|body: axum::body::Bytes| async move {
                // 顺带验证 request_id 以 base64 回传
                let f = parse_form(&body);
                assert_eq!(f["client_id"], "1");
                assert_eq!(
                    f["request_id"],
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [1u8])
                );
                (
                    eresult_header(),
                    axum::Json(serde_json::json!({
                        "response": { "new_challenge": "https://s.team/q/1/rotated" }
                    })),
                )
            }),
        );
        let base = serve(app).await;
        let client = SteamClient::new(None, false).unwrap();
        let res = poll_status_at(&base, &client, 1, &[1])
            .await
            .expect("轮询应成功")
            .expect("有新二维码必须返回 Some");
        assert!(res.access_token.is_empty());
        assert_eq!(res.new_challenge, "https://s.team/q/1/rotated");
    }

    #[tokio::test]
    async fn poll_status_waiting_means_none() {
        // 真实接口「等待确认中」的响应：eresult=1 + {"had_remote_interaction": false}
        let app = axum::Router::new().route(
            "/IAuthenticationService/PollAuthSessionStatus/v1/",
            axum::routing::post(|_body: axum::body::Bytes| async move {
                (
                    eresult_header(),
                    axum::Json(serde_json::json!({
                        "response": { "had_remote_interaction": false }
                    })),
                )
            }),
        );
        let base = serve(app).await;
        let client = SteamClient::new(None, false).unwrap();
        let res = poll_status_at(&base, &client, 1, &[1]).await.expect("轮询应成功");
        assert!(res.is_none(), "等待中应返回 None（继续轮询）");
    }

    #[test]
    fn steamid_from_jwt_decodes_sub() {
        // payload = {"sub":"76561198000000001","iss":"steam"}（base64url 无填充）
        let payload = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            br#"{"sub":"76561198000000001","iss":"steam"}"#,
        );
        let token = format!("eyJhbGciOiJSUzI1NiJ9.{payload}.fakesig");
        assert_eq!(steamid_from_jwt(&token), Some(76561198000000001));
        assert_eq!(steamid_from_jwt("not-a-jwt"), None);
        assert_eq!(steamid_from_jwt(""), None);
    }
}
