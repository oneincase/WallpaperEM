//! 订阅同步：读取登录账号的工坊订阅列表（网页会话，见 steam/auth.rs），
//! 前端拿到列表后可一键全量下载或多选下载（下载本身走既有 download_enqueue）。
//!
//! 与 steamcmd 下载通道的关系：共用「设置 → 账号」里的账号密码做首次网页登录
//! （或扫码登录，完全不碰密码），之后网页会话 token 独立持久化
//! （secure_store session.dat，ajaxrefresh 续期），同步订阅免密免验证码。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::db;
use crate::secure_store;
use crate::steam::auth;
use crate::steam::details::get_item_details;
use crate::steam::SteamClient;

/// 等待手机确认/验证码的轮询总预算
const POLL_BUDGET: Duration = Duration::from_secs(100);
/// 详情元数据缓存有效期（与 workshop.rs 的 DETAIL_TTL_MS 一致）
const DETAIL_TTL_MS: i64 = 24 * 3600 * 1000;
/// PendingAuth.code_type 的特殊值：扫码登录会话（不是验证码会话，靠轮询推进）
const CODE_TYPE_QR: i32 = -1;
/// 扫码轮询单次预算（前端定时器循环调用，单次阻塞太久会拖住 UI 取消操作）
const QR_POLL_BUDGET: Duration = Duration::from_secs(8);

/// 进行中的登录会话（等待用户输验证码/在手机上确认时跨命令保存）
pub struct PendingAuth {
    client_id: u64,
    request_id: Vec<u8>,
    steamid: u64,
    interval_sec: f32,
    /// 需要验证码时的码型（GUARD_DEVICE_CODE / GUARD_EMAIL_CODE）；
    /// 0 = 非验证码等待；CODE_TYPE_QR(-1) = 扫码登录会话
    code_type: i32,
    /// 扫码会话当前要展示的二维码 URL（服务端会轮换，轮询可能下发新的）
    challenge_url: String,
    /// Steam 是否也给「手机 App 点确认」通道（allowed_confirmations 里的
    /// DeviceConfirmation/EmailConfirmation）。手机令牌账号通常同时给出验证码
    /// 与 App 确认两条通道：本字段为 true 时，即使已经进了验证码页，也要
    /// 短轮询看看用户是不是已经在手机上点了「允许」
    device_confirmation: bool,
}

/// Tauri 托管状态
pub struct SubscriptionsState(pub Mutex<Option<PendingAuth>>);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionItem {
    pub id: String,
    pub title: String,
    pub preview_url: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscriptions: Option<i64>,
    /// 已在本地库（不必再下载）
    pub downloaded: bool,
}

/// 登录推进结果：要么拿到可用会话，要么告诉前端该干什么
enum LoginStep {
    Session { steamid: u64, access_token: String },
    /// 需要 Steam Guard 验证码。can_confirm_on_phone = Steam 同时在手机
    /// App 推了「确认登录」，前端可后台轮询等待用户点允许，不必只盯着输入框
    NeedCode {
        code_type: i32,
        message: String,
        can_confirm_on_phone: bool,
    },
    PendingConfirmation { message: String },
}

fn poll_interval(sec: f32) -> Duration {
    Duration::from_secs_f32(sec.clamp(2.0, 10.0))
}

/// 单次轮询登录状态。返回 (拿到 token 则为 Some, 服务端可能轮换后的 client_id)。
/// 服务端会通过 new_client_id 轮换会话 id（验证码提交后常见），调用方必须把
/// 最新 id 写回 PendingAuth，否则下次还用旧 id 会永远等不到——「输入正确验证码
/// 也登录不了」的典型成因
async fn poll_once(
    client: &SteamClient,
    client_id: u64,
    request_id: &[u8],
) -> Result<(Option<auth::PollResult>, u64), String> {
    let mut client_id = client_id;
    if let Some(res) = auth::poll_status(client, client_id, request_id).await? {
        if res.new_client_id != 0 && res.new_client_id != client_id {
            tracing::info!("订阅同步：服务端轮换 client_id {client_id} → {}", res.new_client_id);
            client_id = res.new_client_id;
        }
        // 扫码场景下服务端可能只下发轮换的新二维码（无 token），不算成功
        if !res.access_token.is_empty() && !res.refresh_token.is_empty() {
            return Ok((Some(res), client_id));
        }
    }
    Ok((None, client_id))
}

/// 在预算内轮询登录结果
async fn poll_until_token(
    client: &SteamClient,
    client_id: u64,
    request_id: &[u8],
    interval_sec: f32,
    budget: Duration,
) -> Result<(Option<auth::PollResult>, u64), String> {
    let start = std::time::Instant::now();
    let mut client_id = client_id;
    loop {
        let (res, latest) = poll_once(client, client_id, request_id).await?;
        client_id = latest;
        if res.is_some() {
            return Ok((res, client_id));
        }
        if start.elapsed() >= budget {
            return Ok((None, client_id));
        }
        tokio::time::sleep(poll_interval(interval_sec)).await;
    }
}

fn credentials(app: &AppHandle) -> Result<(String, String, std::path::PathBuf), String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let username = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::get_setting(&conn, "download_username")
    }
    .ok_or("尚未配置下载账号，请到「设置 → 账号」登录 Steam")?;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    let password = secure_store::load(&username, &dir)
        .ok()
        .flatten()
        .ok_or(
            "本地没有该账号的密码（此前靠 steamcmd 缓存免密）。\
             请到「设置 → 账号」重新输入一次账号密码，用于建立订阅同步的网页会话",
        )?;
    Ok((username, password, dir))
}

/// 推进登录：refresh token → 账号密码 →（验证码/手机确认）。
/// 需要用户动作时把会话存进 state，由后续命令继续。
async fn login_step(
    app: &AppHandle,
    client: &SteamClient,
    state: &SubscriptionsState,
) -> Result<LoginStep, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;

    // 1) 已存的网页会话 token 直接续期（ajaxrefresh，官方网页同款；旧 token 过期也能续）。
    //    不校验用户名：扫码登录保存的账号可能与下载账号不同；
    //    且不能放在 credentials() 之后——本地没存密码时网页会话照样该能用
    let mut refresh_err: Option<String> = None;
    if let Some((saved_user, steamid, saved_token)) = secure_store::load_session_any(&dir) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let exp = auth::jwt_exp(&saved_token).unwrap_or(0);
        if exp > now + 3600 {
            // token 还剩 >1 小时：直接用，不续期。
            // 刚扫码/登录完就是这种情况——新 token 绝对有效，没必要立刻续期
            // （曾踩过的坑：每次都续期，续期端点任何风吹草动都会把新登录打挂）
            return Ok(LoginStep::Session {
                steamid,
                access_token: saved_token,
            });
        }
        // 临近过期（<1h）或已过期：走 ajaxrefresh 续期
        match auth::renew_web_session(client, steamid, &saved_token).await {
            Ok(res) => {
                // 续期拿到的新 token 必须存回去（下次续期要用新的）
                if let Err(e) =
                    secure_store::save_session(&saved_user, steamid, &res.access_token, &dir)
                {
                    tracing::warn!("订阅同步：保存续期后的会话 token 失败（{e}）");
                }
                tracing::info!("订阅同步：网页会话续期成功（rtExpiry={}）", res.rt_expiry);
                return Ok(LoginStep::Session {
                    steamid,
                    access_token: res.access_token,
                });
            }
            Err(e) => {
                if exp > now {
                    // 续期被拒但 token 其实还没过期：先用旧的顶一次，别误伤有效会话
                    tracing::warn!("订阅同步：续期失败但旧 token 未过期，本次先用旧 token（{e}）");
                    return Ok(LoginStep::Session {
                        steamid,
                        access_token: saved_token,
                    });
                }
                tracing::warn!("订阅同步：网页会话续期失败（{e}），回退密码登录");
                // 只有「确定被服务器拒绝」才清掉本地会话；网络错误等保留，下次还能再试
                if e.contains("需要重新登录") {
                    secure_store::clear_session(&dir);
                }
                refresh_err = Some(e);
            }
        }
    }

    // 2) 账号密码开新会话
    let (username, password, _dir) = match credentials(app) {
        Ok(c) => c,
        Err(e) => {
            // 没存密码时，续期失败才是真正的失败原因——如实报出，别拿密码错误掩盖
            if let Some(re) = refresh_err {
                return Err(format!(
                    "已保存的登录态续期失败：{re}。请重新扫码登录（本地同时没有可用密码：{e}）"
                ));
            }
            return Err(e);
        }
    };
    let begin = match auth::begin_with_password(client, &username, &password).await {
        Ok(b) => b,
        Err(e) => {
            // 密码也失败时把续期错误一并报出，方便定位到底是哪一环的问题
            if let Some(re) = refresh_err {
                return Err(format!("登录态续期失败：{re}；密码登录同样失败：{e}"));
            }
            return Err(e);
        }
    };

    // 需要 Steam Guard 验证码（手机令牌 / 邮箱码）：存会话，让前端收码
    if let Some(c) = begin
        .allowed
        .iter()
        .find(|c| {
            c.confirmation_type == auth::GUARD_DEVICE_CODE
                || c.confirmation_type == auth::GUARD_EMAIL_CODE
        })
        .cloned()
    {
        let code_type = c.confirmation_type;
        // 手机令牌账号上，Steam 通常同时给出「输验证码」和「手机 App 点确认」两条
        // 通道。记下后者：前端即使已进验证码页，也要能等到用户在手机上点允许
        let can_confirm_on_phone = begin.allowed.iter().any(|c| {
            c.confirmation_type == auth::GUARD_DEVICE_CONFIRMATION
                || c.confirmation_type == auth::GUARD_EMAIL_CONFIRMATION
        });
        *state.0.lock().map_err(|e| e.to_string())? = Some(PendingAuth {
            client_id: begin.client_id,
            request_id: begin.request_id,
            steamid: begin.steamid,
            interval_sec: begin.interval_sec,
            code_type,
            challenge_url: String::new(),
            device_confirmation: can_confirm_on_phone,
        });
        let message = if code_type == auth::GUARD_DEVICE_CODE {
            if can_confirm_on_phone {
                "请输入 Steam 手机令牌上的验证码；也可在 Steam 手机 App 上点「允许」".to_string()
            } else {
                "请输入 Steam 手机令牌上的验证码".to_string()
            }
        } else {
            format!(
                "Steam 已向 {} 发送验证码邮件，请查收",
                if c.associated_message.is_empty() {
                    "你的邮箱"
                } else {
                    &c.associated_message
                }
            )
        };
        return Ok(LoginStep::NeedCode {
            code_type,
            message,
            can_confirm_on_phone,
        });
    }

    // 无需验证码（含手机 App 点「确认」）：轮询等待
    let (res, latest_client_id) = poll_until_token(
        client,
        begin.client_id,
        &begin.request_id,
        begin.interval_sec,
        POLL_BUDGET,
    )
    .await?;
    match res {
        Some(tokens) => {
            // 存 access token（理由同上：refresh token 对网页会话不可用）
            let _ = secure_store::save_session(&username, begin.steamid, &tokens.access_token, &dir);
            Ok(LoginStep::Session {
                steamid: begin.steamid,
                access_token: tokens.access_token,
            })
        }
        None => {
            // 超时：保存会话，前端可继续等待（手机确认可能还没点）
            let needs_app_confirm = begin.allowed.iter().any(|c| {
                c.confirmation_type == auth::GUARD_DEVICE_CONFIRMATION
                    || c.confirmation_type == auth::GUARD_EMAIL_CONFIRMATION
            });
            *state.0.lock().map_err(|e| e.to_string())? = Some(PendingAuth {
                client_id: latest_client_id,
                request_id: begin.request_id,
                steamid: begin.steamid,
                interval_sec: begin.interval_sec,
                code_type: 0,
                challenge_url: String::new(),
                device_confirmation: false,
            });
            Ok(LoginStep::PendingConfirmation {
                message: if needs_app_confirm {
                    "请在 Steam 手机 App 上确认这次登录，确认后点「继续」".to_string()
                } else {
                    "登录确认超时，请点「继续」重试".to_string()
                },
            })
        }
    }
}

/// 已有等待中的会话：继续轮询（手机确认后点「继续」走这里）。
/// 返回 (推进结果, 最新 client_id)
async fn resume_pending(
    client: &SteamClient,
    pending: &PendingAuth,
    username: &str,
    dir: &std::path::Path,
    budget: Duration,
) -> Result<(LoginStep, u64), String> {
    let (res, latest_client_id) = poll_until_token(
        client,
        pending.client_id,
        &pending.request_id,
        pending.interval_sec,
        budget,
    )
    .await?;
    match res {
        Some(tokens) => {
            // 账号名以 Steam 返回的为准（空则回退到下载账号名）。
            // 存 access token（网页会话凭证）：续期走 ajaxrefresh 用它换新，
            // refresh token 对 WebBrowser 平台在 WebAPI 网关不可用（EResult 15）
            let account = if tokens.account_name.is_empty() {
                username
            } else {
                &tokens.account_name
            };
            let _ = secure_store::save_session(account, pending.steamid, &tokens.access_token, dir);
            Ok((
                LoginStep::Session {
                    steamid: pending.steamid,
                    access_token: tokens.access_token,
                },
                latest_client_id,
            ))
        }
        None => Ok((
            LoginStep::PendingConfirmation {
                message: "还在等待确认。请在 Steam 手机 App 上确认登录后再点「继续」".to_string(),
            },
            latest_client_id,
        )),
    }
}

fn step_to_json(step: LoginStep) -> Option<serde_json::Value> {
    match step {
        LoginStep::Session { .. } => None,
        LoginStep::NeedCode {
            code_type,
            message,
            can_confirm_on_phone,
        } => Some(json!({
            "status": "needCode",
            "codeType": if code_type == auth::GUARD_DEVICE_CODE { "device" } else { "email" },
            "message": message,
            "canConfirmOnPhone": can_confirm_on_phone,
        })),
        LoginStep::PendingConfirmation { message } => Some(json!({
            "status": "pendingConfirmation",
            "message": message,
        })),
    }
}

/// 登录（或继续等待中的登录）→ 抓指定页的订阅 id → 批量补元数据 → 标已下载。
/// 分页懒加载：每次只抓一页（30 条），总数随响应返回，前端滚动到底再要下一页
/// ensure_session 的结果：要么拿到可用会话，要么是要透传给前端的状态
enum EnsureSession {
    Session { steamid: u64, access_token: String },
    Status(serde_json::Value),
}

/// 登录状态机的公共部分：等待中的会话续跑 / 重新登录，直到拿到会话或需要用户动作
async fn ensure_session(
    app: &AppHandle,
    client: &SteamClient,
    state: &SubscriptionsState,
) -> Result<EnsureSession, String> {
    // 有等待中的会话先续跑（手机确认场景）
    let pending = state.0.lock().map_err(|e| e.to_string())?.take();
    let step = if let Some(p) = pending {
        // 扫码会话归 subscriptions_qr_poll 管，这里弃掉重新走密码登录
        if p.code_type == CODE_TYPE_QR {
            match login_step(app, client, state).await {
                Ok(s) => s,
                Err(e) if auth::is_invalid_password(&e) => {
                    return Ok(EnsureSession::Status(bad_credentials_json(app, &e)));
                }
                Err(e) => return Err(e),
            }
        } else if p.code_type != 0 {
            let code_type = p.code_type;
            // 手机令牌账号：Steam 同时给了「验证码」和「手机 App 点确认」两条通道。
            // 用户很可能直接在手机上点了允许（而不是回来输码）——先短轮询一轮把
            // 这个会话的 token 捞回来；没捞到就原样退回码页，并告诉前端可以后台
            // 轮询。不做这一步的后果：页面永远停在「请输入令牌验证码」，
            // 用户明明已在手机上允许，却一直登不进去。
            if p.device_confirmation {
                // 只查一次（前端每 ~3s 重复调用）：命令要尽快返回，
                // 避免长时间把等待中的会话从 state 里占住，挡住用户手动输码
                let (res, latest_client_id) = poll_once(client, p.client_id, &p.request_id).await?;
                if let Some(tokens) = res {
                    let (username, _, dir) = credentials(app)?;
                    let account = if tokens.account_name.is_empty() {
                        username
                    } else {
                        tokens.account_name
                    };
                    let _ =
                        secure_store::save_session(&account, p.steamid, &tokens.access_token, &dir);
                    return Ok(EnsureSession::Session {
                        steamid: p.steamid,
                        access_token: tokens.access_token,
                    });
                }
                let step = LoginStep::NeedCode {
                    code_type,
                    can_confirm_on_phone: true,
                    message: if code_type == auth::GUARD_DEVICE_CODE {
                        "请输入 Steam 手机令牌上的验证码；也可在 Steam 手机 App 上点「允许」"
                            .to_string()
                    } else {
                        "请输入 Steam 发送到你邮箱的验证码".to_string()
                    },
                };
                *state.0.lock().map_err(|e| e.to_string())? = Some(PendingAuth {
                    client_id: latest_client_id,
                    ..p
                });
                return Ok(EnsureSession::Status(step_to_json(step).unwrap()));
            }
            let step = LoginStep::NeedCode {
                code_type,
                can_confirm_on_phone: false,
                message: if code_type == auth::GUARD_DEVICE_CODE {
                    "请输入 Steam 手机令牌上的验证码".to_string()
                } else {
                    "请输入 Steam 发送到你邮箱的验证码".to_string()
                },
            };
            *state.0.lock().map_err(|e| e.to_string())? = Some(p);
            return Ok(EnsureSession::Status(step_to_json(step).unwrap()));
        } else {
        let (username, _, dir) = credentials(app)?;
        let (step, latest_client_id) =
            resume_pending(client, &p, &username, &dir, POLL_BUDGET).await?;
        // 没轮到的把会话放回去（带上服务端可能轮换过的 client_id），下次继续
        if !matches!(step, LoginStep::Session { .. }) {
            *state.0.lock().map_err(|e| e.to_string())? = Some(PendingAuth {
                client_id: latest_client_id,
                ..p
            });
        }
        step
        }
    } else {
        match login_step(app, client, state).await {
            Ok(s) => s,
            // 密码不被 Steam 接受：返回结构化状态让前端原地收新密码，而不是一句报错。
            // 典型场景：steamcmd 靠它自己缓存的登录态下载，本地存的密码过期了无人察觉
            Err(e) if auth::is_invalid_password(&e) => {
                return Ok(EnsureSession::Status(bad_credentials_json(app, &e)));
            }
            Err(e) => return Err(e),
        }
    };
    match step {
        LoginStep::Session {
            steamid,
            access_token,
        } => Ok(EnsureSession::Session {
            steamid,
            access_token,
        }),
        other => Ok(EnsureSession::Status(step_to_json(other).unwrap())),
    }
}

/// badCredentials 状态 JSON（附带当前用户名给前端预填）
fn bad_credentials_json(app: &AppHandle, e: &str) -> serde_json::Value {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let username = db
        .lock()
        .ok()
        .and_then(|conn| db::get_setting(&conn, "download_username"));
    json!({
        "status": "badCredentials",
        "message": e,
        "username": username,
    })
}

/// 登录（或继续等待中的登录）→ 抓指定页的订阅 id → 批量补元数据 → 标已下载（内部实现）
async fn page_impl(
    app: &AppHandle,
    client: &SteamClient,
    state: &SubscriptionsState,
    page: u32,
) -> Result<serde_json::Value, String> {
    let (steamid, access_token) = match ensure_session(app, client, state).await? {
        EnsureSession::Session {
            steamid,
            access_token,
        } => (steamid, access_token),
        EnsureSession::Status(v) => return Ok(v),
    };

    let dump_dir = app.path().app_data_dir().ok();
    let (ids, total) =
        auth::fetch_subscribed_page(client, steamid, &access_token, page, dump_dir.as_deref())
            .await?;
    let items = build_items(app, client, &ids).await?;
    Ok(json!({ "status": "ok", "items": items, "total": total, "page": page }))
}

/// 订阅 id 列表 → 元数据（标题/预览/类型/订阅数，24h 缓存 + 批量详情）+ 已下载标记
async fn build_items(
    app: &AppHandle,
    client: &SteamClient,
    ids: &[String],
) -> Result<Vec<SubscriptionItem>, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let mut known = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::find_workshop_items(&conn, ids, DETAIL_TTL_MS)?
    };
    let missing: Vec<String> = ids
        .iter()
        .filter(|id| !known.contains_key(*id))
        .cloned()
        .collect();
    // 详情接口一次 ≤30 个 id
    for chunk in missing.chunks(30) {
        let details = get_item_details(client, chunk).await?;
        let conn = db.lock().map_err(|e| e.to_string())?;
        for d in &details {
            db::upsert_workshop_item(&conn, d)?;
        }
        drop(conn);
        for d in details {
            known.insert(d.id.clone(), d);
        }
    }
    let downloaded: std::collections::HashSet<String> = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT item_id FROM library_items")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    };
    Ok(ids
        .iter()
        .map(|id| {
            let d = known.get(id);
            SubscriptionItem {
                id: id.clone(),
                title: d
                    .map(|d| d.title.clone())
                    .unwrap_or_else(|| format!("工坊物品 {id}")),
                preview_url: d.map(|d| d.preview_url.clone()).unwrap_or_default(),
                r#type: d.map(|d| d.r#type.clone()).unwrap_or_else(|| "unknown".into()),
                subscriptions: d.and_then(|d| d.subscriptions),
                downloaded: downloaded.contains(id),
            }
        })
        .collect())
}

// ---------- Tauri 命令 ----------

/// 订阅同步前置状态：是否已配置账号、是否已有免密网页会话
#[tauri::command]
pub fn subscriptions_status(app: AppHandle) -> Result<serde_json::Value, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let username = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::get_setting(&conn, "download_username")
    };
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let has_session = secure_store::load_session_any(&dir).is_some();
    Ok(json!({
        "configured": username.is_some(),
        "username": username,
        "hasSession": has_session,
    }))
}

/// 只建立网页会话（保存下载凭据时的「双验证」用）：不拉订阅列表，
/// 成功返回 ok；需要验证码/手机确认/重输密码时返回对应状态，
/// 后续由 subscriptions_submit_code / subscriptions_page 接力
#[tauri::command]
pub async fn account_web_login_start(
    app: AppHandle,
    client: tauri::State<'_, SteamClient>,
    state: tauri::State<'_, SubscriptionsState>,
) -> Result<serde_json::Value, String> {
    match ensure_session(&app, &client, &state).await? {
        EnsureSession::Session { .. } => Ok(json!({ "status": "ok" })),
        EnsureSession::Status(v) => Ok(v),
    }
}

/// 拉取一页订阅列表（必要时先登录；需要验证码/手机确认时返回对应状态）。
/// page 从 1 开始；响应带 total（订阅总数）供前端判断是否还有下一页
#[tauri::command]
pub async fn subscriptions_page(
    app: AppHandle,
    client: tauri::State<'_, SteamClient>,
    state: tauri::State<'_, SubscriptionsState>,
    page: u32,
) -> Result<serde_json::Value, String> {
    page_impl(&app, &client, &state, page.max(1)).await
}

/// 提交 Steam Guard 验证码，继续登录并拉取订阅列表
#[tauri::command]
pub async fn subscriptions_submit_code(
    app: AppHandle,
    client: tauri::State<'_, SteamClient>,
    state: tauri::State<'_, SubscriptionsState>,
    code: String,
) -> Result<serde_json::Value, String> {
    // 归一化：去空白 + 大写（手机令牌码全大写；用户可能带空格/小写输入）
    let code: String = code
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_uppercase())
        .collect();
    if code.is_empty() {
        return Err("请输入验证码".into());
    }
    // 后端的「等手机确认」单次轮询可能正好把会话取走在途（窗口 = 一次请求）。
    // 这里短暂等一下它放回，避免用户刚在手机上点了允许、又手动提交验证码时，
    // 被误报「没有等待验证码的登录会话」
    let mut pending = None;
    for _ in 0..8 {
        pending = state.0.lock().map_err(|e| e.to_string())?.take();
        if pending.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let Some(p) = pending else {
        return Err("没有等待验证码的登录会话，请重新开始同步".into());
    };
    if p.code_type == 0 {
        *state.0.lock().map_err(|e| e.to_string())? = Some(p);
        return Err("当前登录不需要验证码".into());
    }
    auth::submit_guard_code(&client, p.client_id, p.steamid, &code, p.code_type).await?;
    // 提交后轮询拿 token；没拿到就存回会话让前端「继续」重试
    let (username, _, dir) = credentials(&app)?;
    let (step, latest_client_id) =
        resume_pending(&client, &p, &username, &dir, POLL_BUDGET).await?;
    if !matches!(step, LoginStep::Session { .. }) {
        // 关键：码已提交过，存回时 code_type 置 0（不再需要验证码）。
        // 否则前端点「继续」会被 page_impl 弹回收码页，用户误以为码不对，
        // 反复输入正确验证码也登不上
        *state.0.lock().map_err(|e| e.to_string())? = Some(PendingAuth {
            client_id: latest_client_id,
            code_type: 0,
            ..p
        });
        return Ok(step_to_json(step).unwrap());
    }
    // 会话已持久化（resume_pending 里保存），列表交给前端的 subscriptions_page(1)，
    // 这样「登录成功」的反馈和列表加载进度都能及时展示
    Ok(json!({ "status": "confirmed" }))
}

/// 开启扫码登录：返回 challengeUrl 给前端渲染二维码，会话存进 state 轮询推进
#[tauri::command]
pub async fn subscriptions_qr_begin(
    client: tauri::State<'_, SteamClient>,
    state: tauri::State<'_, SubscriptionsState>,
) -> Result<serde_json::Value, String> {
    let begin = auth::begin_qr(&client).await?;
    let challenge_url = begin.challenge_url.clone();
    *state.0.lock().map_err(|e| e.to_string())? = Some(PendingAuth {
        client_id: begin.client_id,
        request_id: begin.request_id,
        // QR 会话此时还不知道 steamid（要从轮询拿到的 JWT 里解）
        steamid: 0,
        interval_sec: begin.interval_sec,
        code_type: CODE_TYPE_QR,
        challenge_url: challenge_url.clone(),
        device_confirmation: false,
    });
    Ok(json!({ "status": "qr", "challengeUrl": challenge_url }))
}

/// 扫码轮询：在短预算内等待用户扫码并在手机 App 上确认。
/// 拿到 token 后直接拉订阅列表返回 ok；没拿到则回传（可能已轮换的）二维码继续等
#[tauri::command]
pub async fn subscriptions_qr_poll(
    app: AppHandle,
    client: tauri::State<'_, SteamClient>,
    state: tauri::State<'_, SubscriptionsState>,
) -> Result<serde_json::Value, String> {
    let pending = state.0.lock().map_err(|e| e.to_string())?.take();
    let Some(p) = pending else {
        return Err("没有进行中的扫码登录，请重新开始".into());
    };
    if p.code_type != CODE_TYPE_QR {
        *state.0.lock().map_err(|e| e.to_string())? = Some(p);
        return Err("当前等待中的登录不是扫码会话".into());
    }

    // 短轮询循环：顺带收集服务端轮换下发的新二维码
    let start = std::time::Instant::now();
    let mut latest_challenge = p.challenge_url.clone();
    let tokens = loop {
        match auth::poll_status(&client, p.client_id, &p.request_id).await {
            Ok(Some(res)) => {
                if !res.new_challenge.is_empty() {
                    latest_challenge = res.new_challenge.clone();
                }
                if !res.access_token.is_empty() && !res.refresh_token.is_empty() {
                    break Some(res);
                }
            }
            Ok(None) => {}
            Err(e) => {
                // 会话失效（超时/被踢）：不放回，让前端提示重开
                return Err(e);
            }
        }
        if start.elapsed() >= QR_POLL_BUDGET {
            break None;
        }
        tokio::time::sleep(poll_interval(p.interval_sec)).await;
    };

    let Some(tokens) = tokens else {
        // 还没确认：把（可能已更新的）二维码存回会话，前端继续轮询
        *state.0.lock().map_err(|e| e.to_string())? = Some(PendingAuth {
            challenge_url: latest_challenge.clone(),
            ..p
        });
        return Ok(json!({ "status": "qr", "challengeUrl": latest_challenge }));
    };

    // 扫码成功：账号名以 Steam 返回的为准（可能与下载账号不同），持久化会话。
    // QR 的 Begin 响应不带 steamid，从 access_token 的 JWT sub 字段解出
    let steamid = auth::steamid_from_jwt(&tokens.access_token)
        .ok_or("扫码登录成功，但无法从令牌解析 steamid（令牌格式异常）")?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let account = if tokens.account_name.is_empty() {
        "steam"
    } else {
        tokens.account_name.as_str()
    };
    // 存 access token（网页会话凭证，续期走 ajaxrefresh）。
    // 保存失败必须显式告知：否则用户下次打开又要重新扫码
    secure_store::save_session(account, steamid, &tokens.access_token, &dir)
        .map_err(|e| format!("扫码登录成功，但保存登录凭证失败: {e}"))?;
    tracing::info!("订阅同步：扫码登录成功，已保存账号 {account} 的登录凭证");
    // 列表交给前端的 subscriptions_page(1)（分页懒加载），
    // 这里立刻返回让前端马上给出「登录成功」反馈
    Ok(json!({ "status": "confirmed" }))
}

/// 清除保存的网页会话（下次同步需重新验证）
#[tauri::command]
pub fn subscriptions_logout(app: AppHandle) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    secure_store::clear_session(&dir);
    Ok(())
}
