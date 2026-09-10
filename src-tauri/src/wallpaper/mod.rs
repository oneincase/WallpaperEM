//! 壁纸引擎（T3）：多显示器桌面层窗口 + 会话持久化 + 睡眠暂停 + 轮播播放列表
//!
//! 每显示器一个 Tauri 桌面窗口（label = "wallpaper-<displayId>"），渲染器页
//! 经 URL query 注入配置；屏幕布局变化由后台监控任务同步（2s）；
//! 显示器睡眠由 CGDisplayIsAsleep 轮询（5s）驱动暂停/恢复。

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod platform;
pub mod pointer;

use crate::audio_capture;
use crate::db;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::content_server::ContentServerState;

pub const DEFAULT_FIT: &str = "cover";
/// 清晰度（有效 devicePixelRatio 的封顶）：越低越省内存（GPU 画布/纹理）。
/// 三档：0.8 省电 / 1 标准 / 2 高清。
///
/// 上限 2.0 而非更高：实际生效值是 `min(window.devicePixelRatio, cap)`，
/// 而 Retina 的 dpr 就是 2 —— 更高的档位在任何 Mac 上都会被压到 2，只会让
/// 下拉框多出"选了没变化"的空档位（此前 3x/4x/5x 就是这个问题）。
/// 缩放模式下 WebKit 自己把 dpr 报成 1，也不需要更高的 cap 去补。
pub const RENDER_DPR_MIN: f32 = 0.8;
pub const RENDER_DPR_MAX: f32 = 2.0;
pub const DEFAULT_RENDER_DPR: f32 = 1.0;
/// 场景壁纸帧率上限（帧/秒）：越低 GPU 占用越低。可选 30 / 60 / 120，默认 60。
pub const DEFAULT_SCENE_FPS: u32 = 60;

fn default_type() -> String {
    "canvas".into()
}
fn default_fit() -> String {
    DEFAULT_FIT.into()
}
fn default_render_dpr() -> f32 {
    DEFAULT_RENDER_DPR
}
fn default_scene_fps() -> u32 {
    DEFAULT_SCENE_FPS
}

/// 全局壁纸显示模式（覆盖到每次应用/恢复），非法值回退到默认 cover。
fn global_fit(conn: Option<&Connection>) -> String {
    let fit = conn.and_then(|c| db::get_setting(c, "wallpaper_fit"));
    match fit.as_deref() {
        Some("contain") | Some("stretch") | Some("cover") => {
            fit.unwrap_or_else(|| DEFAULT_FIT.into())
        }
        _ => DEFAULT_FIT.into(),
    }
}

/// 全局渲染分辨率上限（有效 dpr 封顶），读取设置 `wallpaper_render_dpr`，非法值回退到默认。
fn global_render_dpr(conn: Option<&Connection>) -> f32 {
    let raw = conn.and_then(|c| db::get_setting(c, "wallpaper_render_dpr"));
    let parsed = raw
        .as_deref()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .unwrap_or(DEFAULT_RENDER_DPR);
    parsed.clamp(RENDER_DPR_MIN, RENDER_DPR_MAX)
}

/// 全局场景帧率上限（读设置 `wallpaper_scene_fps`），只允许 30/60/120，非法值回退 60。
pub(crate) fn global_scene_fps(conn: Option<&Connection>) -> u32 {
    let raw = conn.and_then(|c| db::get_setting(c, "wallpaper_scene_fps"));
    let parsed = raw
        .as_deref()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_SCENE_FPS);
    match parsed {
        30 | 60 | 120 => parsed,
        _ => DEFAULT_SCENE_FPS,
    }
}

// ---------- 每壁纸播放设置（对标 WE 的「壁纸配置」）----------
//
// WE 的播放设置是**按壁纸记忆**的：A 壁纸调过音量，切到 B 再切回 A 仍是那个音量。
// 我们此前只有全局单例（设置页那几项），换壁纸就丢。
//
// 存储沿用作者属性同一套路：settings 表键 `play_cfg:{item_id}`，值为 JSON 对象。
// 不新建表的理由是这里天然是稀疏的键值覆盖 —— 只存"用户显式改过的项"，
// 没有的字段回落全局默认。新建表反而要处理"行存在但字段为 NULL"的三态。
//
// 语义严格是三态，不能简化成两态：
//   字段缺失   → 跟随全局（用户没碰过）
//   字段有值   → 本壁纸专属，覆盖全局
// 所以用 Option<T> 而不是"等于默认值就算跟随" —— 后者会让"用户特意设成与
// 当前全局相同的值"在全局改动后被意外带走。

/// 单张壁纸的播放设置覆盖。None 字段 = 跟随全局默认。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemPlayConfig {
    /// 显示模式（cover/contain/stretch）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<String>,
    /// 清晰度（有效 dpr 封顶）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_dpr: Option<f32>,
    /// 帧率限制
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_fps: Option<u32>,
    /// 音量 0..1（0 即静音）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
}

fn play_cfg_key(item_id: &str) -> String {
    format!("play_cfg:{item_id}")
}

/// 读某壁纸的播放设置覆盖（无记录或解析失败 → 全跟随全局）
pub(crate) fn item_play_config(conn: &Connection, item_id: &str) -> ItemPlayConfig {
    db::get_setting(conn, &play_cfg_key(item_id))
        .and_then(|s| serde_json::from_str::<ItemPlayConfig>(&s).ok())
        .unwrap_or_default()
}

/// 写某壁纸的播放设置覆盖。全字段为 None 时删除该键（回到「完全跟随全局」）
fn set_item_play_config(
    conn: &Connection,
    item_id: &str,
    v: &ItemPlayConfig,
) -> Result<(), String> {
    let key = play_cfg_key(item_id);
    let empty =
        v.fit.is_none() && v.render_dpr.is_none() && v.scene_fps.is_none() && v.volume.is_none();
    if empty {
        // 留一个空 JSON 会让「跟随全局」和「曾经改过又还原」在 DB 里长得不一样，
        // 后续想按 key 存在性做统计就会错；直接删干净
        conn.execute(
            "DELETE FROM settings WHERE key = ?1",
            rusqlite::params![key],
        )
        .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let json = serde_json::to_string(v).map_err(|e| e.to_string())?;
    db::set_setting(conn, &key, &json).map_err(|e| e.to_string())
}

/// 把「全局默认 + 本壁纸覆盖」合成进配置。
///
/// 取代原先分散的 apply_global_fit / _render_dpr / _scene_fps 三连调用 ——
/// 那三个只认全局，加每壁纸覆盖时三处调用点都得改，容易漏一处导致
/// 「应用时生效、睡眠恢复后又变回全局」这类难查的不一致。
fn apply_play_config(app: &AppHandle, cfg: &mut WallpaperConfig, item_id: Option<&str>) {
    let db = app.try_state::<Arc<Mutex<Connection>>>();
    let Some(state) = db else {
        cfg.fit = DEFAULT_FIT.into();
        cfg.render_dpr = DEFAULT_RENDER_DPR;
        cfg.scene_fps = DEFAULT_SCENE_FPS;
        return;
    };
    let Ok(conn) = state.lock() else {
        cfg.fit = DEFAULT_FIT.into();
        cfg.render_dpr = DEFAULT_RENDER_DPR;
        cfg.scene_fps = DEFAULT_SCENE_FPS;
        return;
    };
    // 先铺全局
    cfg.fit = global_fit(Some(&conn));
    cfg.render_dpr = global_render_dpr(Some(&conn));
    cfg.scene_fps = global_scene_fps(Some(&conn));
    // 再叠本壁纸覆盖
    if let Some(id) = item_id {
        let ov = item_play_config(&conn, id);
        if let Some(f) = ov.fit.as_deref() {
            if matches!(f, "cover" | "contain" | "stretch") {
                cfg.fit = f.to_string();
            }
        }
        if let Some(d) = ov.render_dpr {
            cfg.render_dpr = d.clamp(RENDER_DPR_MIN, RENDER_DPR_MAX);
        }
        if let Some(f) = ov.scene_fps {
            if matches!(f, 30 | 60 | 120) {
                cfg.scene_fps = f;
            }
        }
        if let Some(v) = ov.volume {
            // muted 是 renderer 侧的开关；音量 0 即静音，非 0 则取消静音
            cfg.muted = v <= 0.0;
        }
    }
}

/// 读某壁纸的播放设置（给前端：同时给出覆盖值与当前全局默认，便于显示「跟随全局」）
#[tauri::command(rename = "wallpaper_item_play_config")]
pub fn item_play_config_get(app: AppHandle, item_id: String) -> Result<serde_json::Value, String> {
    let db = app
        .try_state::<Arc<Mutex<Connection>>>()
        .ok_or("DB 未就绪")?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    let ov = item_play_config(&conn, &item_id);
    Ok(serde_json::json!({
        "override": ov,
        "globals": {
            "fit": global_fit(Some(&conn)),
            "renderDpr": global_render_dpr(Some(&conn)),
            "sceneFps": global_scene_fps(Some(&conn)),
        }
    }))
}

/// 写某壁纸的播放设置并立即生效（该壁纸正在播放时才重下发）
#[tauri::command(rename = "wallpaper_item_play_config_set")]
pub fn item_play_config_set(
    app: AppHandle,
    item_id: String,
    config: ItemPlayConfig,
) -> Result<(), String> {
    {
        let db = app
            .try_state::<Arc<Mutex<Connection>>>()
            .ok_or("DB 未就绪")?;
        let conn = db.lock().map_err(|e| e.to_string())?;
        set_item_play_config(&conn, &item_id, &config)?;
    }
    // 只有这张壁纸正在某个屏幕上播放时才需要即时下发；否则下次应用时自然生效
    let playing = active_items(app.clone()).unwrap_or_default();
    if !playing.iter().any(|i| i == &item_id) {
        return Ok(());
    }
    if let Some(f) = config.fit.as_deref() {
        eval_all(
            &app,
            &format!(
                "window.__wp && window.__wp.setFit({})",
                serde_json::json!(f)
            ),
        );
    }
    if let Some(d) = config.render_dpr {
        let d = d.clamp(RENDER_DPR_MIN, RENDER_DPR_MAX);
        eval_all(
            &app,
            &format!("window.__wp && window.__wp.setRenderDpr({d})"),
        );
    }
    if let Some(f) = config.scene_fps {
        eval_all(
            &app,
            &format!("window.__wp && window.__wp.setSceneFps({f})"),
        );
    }
    if let Some(v) = config.volume {
        let v = v.clamp(0.0, 1.0);
        eval_all(&app, &format!("window.__wp && window.__wp.setVolume({v})"));
    }
    Ok(())
}

fn default_muted() -> bool {
    true
}
fn default_loop() -> bool {
    true
}

/// 渲染器启动配置（serde camelCase，与 renderer/src/main.ts 对齐）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperConfig {
    /// canvas | video | gif | web | scene | image
    #[serde(default = "default_type")]
    pub r#type: String,
    #[serde(default)]
    pub src: Option<String>,
    /// cover（等比铺满裁切，默认）| contain（等比留边）| stretch（拉伸，旧行为）
    #[serde(default = "default_fit")]
    pub fit: String,
    /// 渲染分辨率上限（有效 dpr 封顶），越低越省内存
    #[serde(default = "default_render_dpr")]
    pub render_dpr: f32,
    /// 场景壁纸帧率上限（30/60/120），越低 GPU 占用越低
    #[serde(default = "default_scene_fps")]
    pub scene_fps: u32,
    #[serde(default = "default_muted")]
    pub muted: bool,
    #[serde(default = "default_loop")]
    pub r#loop: bool,
    /// 内容服务器基址（scene/web 资源拉取；由引擎注入）
    #[serde(default)]
    pub media_base: Option<String>,
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        Self {
            r#type: default_type(),
            src: None,
            fit: default_fit(),
            render_dpr: default_render_dpr(),
            scene_fps: default_scene_fps(),
            muted: default_muted(),
            r#loop: default_loop(),
            media_base: None,
        }
    }
}

pub struct WallpaperEngineState {
    /// label -> config（当前各屏壁纸）
    pub windows: Mutex<HashMap<String, WallpaperConfig>>,
    /// 最近一次会话配置（显示器 ID 变更/新增屏时作为恢复兜底）
    pub default: Mutex<Option<WallpaperConfig>>,
    pub paused: Mutex<bool>,
    /// 「自动暂停」自己挂上的暂停（区别于用户手动暂停）：回到桌面时只恢复
    /// 这个标志置位的暂停，用户手动暂停不受前台切换影响
    pub auto_paused: Mutex<bool>,
}

impl Default for WallpaperEngineState {
    fn default() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            default: Mutex::new(None),
            paused: Mutex::new(false),
            auto_paused: Mutex::new(false),
        }
    }
}

/// 初始化：管理状态 + 恢复会话 + 启动监控任务（屏幕布局 2s / 睡眠 5s）
pub fn init(app: &AppHandle) -> tauri::Result<()> {
    app.manage(WallpaperEngineState::default());
    restore_sessions(app);
    // 等待内容服务器端口就绪（最长 3s），壁纸窗口从内容服务器同源加载渲染器页
    for _ in 0..30 {
        if let Some(st) = app.try_state::<Arc<Mutex<u16>>>() {
            if let Ok(g) = st.lock() {
                if *g > 0 {
                    break;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // 音频可视化开启时，等系统音频捕获就绪再创建壁纸窗口（壁纸页在服务时刻
    // 读取 systemAudio 快照决定是否订阅频谱流）。有界等待：失败/超时照常渲染，
    // 页面内 shim 对 SSE 的重连订阅可自愈。
    if audio_capture::wait_until_ready(app, std::time::Duration::from_secs(6)) {
        tracing::info!("audio capture ready; creating wallpaper windows");
    } else {
        tracing::info!(
            "audio capture not ready (disabled/failed/slow); relying on shim SSE reconnect"
        );
    }
    ensure_windows(app);
    start_monitor(app);
    // 自动暂停（默认关）：监听前台应用切换，切到非桌面暂停、回桌面恢复
    // （仅 macOS 有前台应用观察者；Linux 后端是空实现，开关暂不生效果详见 linux.rs）
    #[cfg(target_os = "linux")]
    platform::store_app_handle(app);
    platform::start_auto_pause_observer(app);
    // 交互态下点桌面不一定触发前台切换（壁纸窗无边框不能成为 key），
    // 补一条「点击落在壁纸窗口 = 回到桌面」的直接恢复信号（仅 macOS 有实现）
    platform::start_desktop_click_monitor(app);
    // 指针注入：非交互态（壁纸在图标下方）收不到真实鼠标，靠轮询系统光标补上
    pointer::start(app, current_interactive(app));
    tracing::info!("wallpaper engine ready");
    Ok(())
}

/// 从 wallpaper_sessions 表恢复各屏壁纸
fn restore_sessions(app: &AppHandle) {
    let Some(db) = app.try_state::<Arc<Mutex<rusqlite::Connection>>>() else {
        return;
    };
    let restored: (HashMap<String, WallpaperConfig>, Option<WallpaperConfig>) = {
        let conn = match db.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut stmt = match conn.prepare(
            "SELECT display_id, config_json FROM wallpaper_sessions ORDER BY updated_at DESC",
        ) {
            Ok(s) => s,
            Err(_) => return,
        };
        let rows =
            match stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                Ok(rows) => rows,
                Err(_) => return,
            };
        let mut restored: HashMap<String, WallpaperConfig> = HashMap::new();
        let mut most_recent: Option<WallpaperConfig> = None;
        for row in rows.flatten() {
            if let Ok(cfg) = serde_json::from_str::<WallpaperConfig>(&row.1) {
                // 查询按 updated_at DESC，第一行即全局最近一次应用
                if most_recent.is_none() {
                    most_recent = Some(cfg.clone());
                }
                restored.entry(row.0).or_insert(cfg);
            }
        }
        (restored, most_recent)
    };
    let (restored, most_recent) = restored;
    let count = restored.len();
    if let Some(st) = app.try_state::<WallpaperEngineState>() {
        let mut windows = st.windows.lock().unwrap();
        for (display_id, cfg) in restored {
            windows.insert(format!("wallpaper-{display_id}"), cfg);
        }
        *st.default.lock().unwrap() = most_recent;
    }
    tracing::info!("wallpaper sessions restored: {count}");
}

/// 确保每个活动显示器都有壁纸窗口（创建/缩放/回收）
fn ensure_windows(app: &AppHandle) {
    ensure_windows_inner(app, platform::display_asleep());
}

/// `display_asleep` 由调用方传入：monitor 用「CG 报告 && 音频样本停止流动」的
/// 复合判定（CGDisplayIsAsleep 的进程内状态在合盖唤醒后可能卡死在 true，
/// 样本恢复流动即证明系统实际已唤醒）。
fn ensure_windows_inner(app: &AppHandle, display_asleep: bool) {
    // 显示器睡眠/唤醒切换期间不做任何窗口增删：此时 CGGetActiveDisplayList
    // 可能返回空列表（显示器从「活动」列表暂时消失），若照常执行下方清理逻辑，
    // 会把所有壁纸窗口误判为「已断开的显示器」全部销毁 —— 主窗口若也处于闲置
    // 释放状态，最后一个窗口关闭就会触发 Tauri 默认行为退出整个进程
    // （表现为「休眠后壁纸软件退出」）。醒来后由下一轮 tick 恢复同步。
    if display_asleep {
        return;
    }
    let screens = platform::active_screens();
    // 同理：空列表只说明显示器暂时不可枚举（睡眠/热插拔过渡），绝不能当作
    // 「用户拔掉了全部显示器」去销毁窗口；仅在确认还有显示器时才做增删。
    if screens.is_empty() {
        return;
    }
    let state = match app.try_state::<WallpaperEngineState>() {
        Some(s) => s,
        None => return,
    };
    let configs = state.windows.lock().unwrap().clone();
    // 显示器 ID 变更/新增屏时，用最近一次会话配置兜底，保证壁纸仍能恢复
    let default_cfg = state.default.lock().unwrap().clone();

    // 需要的 label 集合
    let mut desired: HashMap<String, (u32, f64, f64, f64, f64)> = HashMap::new();
    for s in &screens {
        desired.insert(format!("wallpaper-{}", s.id), (s.id, s.x, s.y, s.w, s.h));
    }

    // 移除已断开的显示器窗口
    let existing_labels: Vec<String> = configs.keys().cloned().collect();
    for label in &existing_labels {
        if !desired.contains_key(label) {
            if let Some(w) = app.get_webview_window(label) {
                let _ = w.destroy();
            }
            if let Ok(mut windows) = state.windows.lock() {
                windows.remove(label);
            }
        }
    }

    // 创建/缩放窗口
    for (label, (id, x, y, w, h)) in &desired {
        let cfg = configs
            .get(label)
            .cloned()
            .or_else(|| default_cfg.clone())
            .unwrap_or_default();
        match app.get_webview_window(label) {
            Some(win) => {
                platform::set_frame(&win, *x, *y, *w, *h);
            }
            None => {
                if let Err(e) = create_desktop_window(app, label, &cfg, (*x, *y, *w, *h)) {
                    tracing::error!("create wallpaper window {label} failed: {e}");
                    continue;
                }
            }
        }
        let _ = id;
    }
}

fn media_base(app: &AppHandle) -> Option<String> {
    // 真实端口在 Arc<Mutex<u16>>（服务器绑定后异步写入）；ContentServerState.port 恒为占位 0
    let port = app
        .try_state::<Arc<Mutex<u16>>>()?
        .lock()
        .ok()
        .map(|g| *g)?;
    if port == 0 {
        return None;
    }
    let state = app.try_state::<ContentServerState>()?;
    Some(format!("http://127.0.0.1:{port}/media/{}", state.token))
}

/// web 壁纸站点根基址（绝对路径引用可解析）
fn web_base(app: &AppHandle) -> Option<String> {
    let port = app
        .try_state::<Arc<Mutex<u16>>>()?
        .lock()
        .ok()
        .map(|g| *g)?;
    if port == 0 {
        return None;
    }
    let state = app.try_state::<ContentServerState>()?;
    Some(format!("http://127.0.0.1:{port}/web/{}", state.token))
}

/// 修复会话恢复后 src 中过期的内容服务器 token。
///
/// 内容服务器每次启动生成新的随机 token；持久化的 `wallpaper_sessions.config_json`
/// 里的 `src`（video/gif/image/web）会带上旧 token，App 重启后该 URL 已失效（401）。
/// 这里把由本内容服务器派发的 URL 重写为当前基址 + 保留的 item_id/文件名部分。
/// 仅处理 `http://127.0.0.1:<port>/<media|web>/<token>/<item_id>/...` 形态；
/// 外部 URL 与相对路径（如原型面板的 /test-media/...）原样保留。
fn refresh_src(app: &AppHandle, cfg: &mut WallpaperConfig) {
    let Some(src) = cfg.src.clone() else { return };
    if !src.starts_with("http://127.0.0.1:") {
        return;
    }
    let marker = if cfg.r#type == "web" {
        "/web/"
    } else {
        "/media/"
    };
    let Some(idx) = src.find(marker) else { return };
    let after = &src[idx + marker.len()..]; // <token>/<item_id>/<rest...>
    let Some(rest) = after.splitn(2, '/').nth(1) else {
        return;
    }; // <item_id>/<rest...>
    let base = if cfg.r#type == "web" {
        web_base(app)
    } else {
        media_base(app)
    };
    let Some(base) = base else { return };
    cfg.src = Some(format!("{base}/{rest}"));
}

/// 从配置里反解 item_id，供每壁纸播放设置取覆盖值。
///
/// 三处装配点（新建窗口 / 主线程应用 / 强制重载）手上只有 `WallpaperConfig`，
/// 没有独立的 item_id 参数。而 src 本身就带着它：
///   scene → src 直接是 item_id
///   web / 媒体 → `http://127.0.0.1:<port>/<web|media>/<token>/<item_id>/...`
/// 拿不到（外部 URL、原型面板的相对路径、canvas 演示）时返回 None，
/// 调用方退化为"只用全局默认"，与加这个特性之前的行为一致。
fn item_id_of(cfg: &WallpaperConfig) -> Option<String> {
    let src = cfg.src.as_deref()?;
    if cfg.r#type == "scene" {
        // 库形态的 scene src 就是 item_id（纯数字或本地导入的目录名）
        if !src.contains('/') && !src.is_empty() {
            return Some(src.to_string());
        }
    }
    if !src.starts_with("http://127.0.0.1:") {
        return None;
    }
    let marker = if cfg.r#type == "web" {
        "/web/"
    } else {
        "/media/"
    };
    let idx = src.find(marker)?;
    let after = &src[idx + marker.len()..]; // <token>/<item_id>/<rest...>
    let mut it = after.splitn(3, '/');
    let _token = it.next()?;
    let id = it.next()?;
    if id.is_empty() {
        return None;
    }
    Some(id.to_string())
}

fn create_desktop_window(
    app: &AppHandle,
    label: &str,
    cfg: &WallpaperConfig,
    frame: (f64, f64, f64, f64),
) -> Result<WebviewWindow, String> {
    let mut cfg = cfg.clone();
    cfg.media_base = media_base(app);
    // 会话恢复来的 src 可能带上次运行的过期 token，用当前基址重写
    refresh_src(app, &mut cfg);
    // 全局默认 + 本壁纸覆盖（WE 的播放设置是按壁纸记忆的）
    let item = item_id_of(&cfg);
    apply_play_config(app, &mut cfg, item.as_deref());
    let query = config_query_with_audio(&cfg, content_token(app).as_deref());
    // 渲染器页与媒体同源（内容服务器），消除跨源 fetch 限制
    let port: u16 = match app.try_state::<Arc<Mutex<u16>>>() {
        Some(s) => match s.lock() {
            Ok(g) => *g,
            Err(_) => 0,
        },
        None => 0,
    };
    let url = if port > 0 {
        let parsed: url::Url = format!("http://127.0.0.1:{port}/renderer/index.html{query}")
            .parse()
            .map_err(|e: url::ParseError| format!("无效的渲染器 URL: {e}"))?;
        WebviewUrl::External(parsed)
    } else {
        WebviewUrl::App(format!("renderer/index.html{query}").into())
    };
    let window = WebviewWindowBuilder::new(app, label, url)
        .title("")
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .visible(false)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .closable(false)
        .skip_taskbar(true)
        .focused(false)
        .build()
        .map_err(|e| e.to_string())?;

    platform::apply_desktop_window(&window, frame, current_interactive(app));

    window.show().map_err(|e| e.to_string())?;
    // 黑屏加固：窗口是 visible(false) 创建的，建窗瞬间 apply 时 occlusionState
    // 尚无 visible 位（实测 8192），WebKit 可能把页面判为不可见而停帧（黑屏）。
    // 按既有经验「show 之后重设层级 + orderFrontRegardless」，show 后再 apply
    // 一次（幂等），此时窗口已可见，遮挡态与合成层级都被矫正。
    platform::apply_desktop_window(&window, frame, current_interactive(app));
    tracing::info!("wallpaper window {label} created: {cfg:?}");
    Ok(window)
}

fn apply_on_main(
    app: &AppHandle,
    display_id: Option<String>,
    cfg: WallpaperConfig,
    item_id: Option<&str>,
) -> Result<(), String> {
    let screens = platform::active_screens();
    let targets: Vec<(String, (f64, f64, f64, f64))> = screens
        .iter()
        .filter(|s| match &display_id {
            Some(id) => &s.id.to_string() == id,
            None => true,
        })
        .map(|s| (format!("wallpaper-{}", s.id), (s.x, s.y, s.w, s.h)))
        .collect();
    if targets.is_empty() {
        return Err("未找到目标显示器".into());
    }

    let state = app
        .try_state::<WallpaperEngineState>()
        .ok_or("引擎未就绪")?;
    let db = app
        .try_state::<Arc<Mutex<rusqlite::Connection>>>()
        .ok_or("DB 未就绪")?;

    for (label, frame) in &targets {
        let window = match app.get_webview_window(label) {
            Some(w) => w,
            None => create_desktop_window(app, label, &cfg, *frame).map_err(|e| e.to_string())?,
        };
        platform::apply_desktop_window(&window, *frame, current_interactive(app));

        let mut cfg2 = cfg.clone();
        cfg2.media_base = media_base(app);
        let item2 = item_id_of(&cfg2);
        apply_play_config(app, &mut cfg2, item2.as_deref());
        let js = format!(
            "window.__wp && window.__wp.setWallpaper({})",
            serde_json::to_string(&cfg2).map_err(|e| e.to_string())?
        );
        window.eval(&js).map_err(|e| e.to_string())?;
        state
            .windows
            .lock()
            .unwrap()
            .insert(label.clone(), cfg2.clone());

        // 会话持久化（item_id 供「已应用」标识 + 未来按条目恢复）
        if let Ok(conn) = db.lock() {
            let display_id_key = label.strip_prefix("wallpaper-").unwrap_or(label);
            let _ = conn.execute(
                "INSERT INTO wallpaper_sessions(display_id, item_id, config_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(display_id) DO UPDATE SET item_id = ?2, config_json = ?3, updated_at = ?4",
                rusqlite::params![
                    display_id_key,
                    item_id,
                    serde_json::to_string(&cfg2).unwrap_or_default(),
                    chrono::Utc::now().timestamp()
                ],
            );
        }
    }

    // 系统静态壁纸同步（默认开）：抽首帧/代表帧设为系统桌面壁纸，
    // 锁屏/引擎未运行时与桌面视觉一致。失败只记日志，不影响应用结果
    crate::system_wallpaper::sync_after_apply(app, &cfg.r#type, cfg.src.as_deref(), item_id);

    Ok(())
}

fn eval_all(app: &AppHandle, js: &str) {
    let state = match app.try_state::<WallpaperEngineState>() {
        Some(s) => s,
        None => return,
    };
    let labels: Vec<String> = state.windows.lock().unwrap().keys().cloned().collect();
    for label in labels {
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.eval(js);
        }
    }
}

/// 通知所有壁纸窗口切换系统音频源。
///
/// 开启时下发内容服务器 token，渲染器订阅 /audio-stream SSE 并把频谱注入库；
/// 关闭时渲染器回落到库的内置模拟源。**不重挂壁纸** —— 库的音频泵逐帧选源，
/// setAudio 可在任意时刻生效，重挂会让场景重新下载解析上百 MB 的 pkg。
pub fn notify_system_audio(app: &AppHandle, enabled: bool) {
    let token = content_token(app).unwrap_or_default();
    let js = if enabled && !token.is_empty() {
        format!(
            "window.__wp && window.__wp.setSystemAudio(true, {})",
            serde_json::to_string(&token).unwrap_or_else(|_| "\"\"".into())
        )
    } else {
        "window.__wp && window.__wp.setSystemAudio(false)".to_string()
    };
    eval_all(app, &js);
}

/// 是否开启「隐藏图标」（壁纸窗口在桌面图标之上并接收鼠标）；默认关闭
fn current_interactive(app: &AppHandle) -> bool {
    let Some(db) = app.try_state::<Arc<Mutex<rusqlite::Connection>>>() else {
        return false;
    };
    let conn = match db.lock() {
        Ok(c) => c,
        Err(_) => return false,
    };
    db::get_setting(&conn, "wallpaper_interactive")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

/// 监控 tick 的自愈动作
#[derive(Default, Clone, Copy)]
struct TickActions {
    /// 音频捕获停滞（RUNNING 相位序号冻结）或 FAILED 相位到重试间隔：重启捕获
    restart_audio: bool,
    /// web 壁纸页僵死（SSE 客户端数为 0）：用当前配置强制重载壁纸页
    reload_wallpapers: bool,
}

fn start_monitor(app: &AppHandle) {
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut was_asleep = false;
        let mut last_audio_seq: u64 = 0;
        let mut audio_stale_ticks: u32 = 0;
        let mut failed_retry_ticks: u32 = 0;
        let mut web_stall_ticks: u32 = 0;
        let mut ticks: u64 = 0;
        tracing::debug!("wallpaper monitor started");
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            ticks += 1;
            // 单个 tick 的 panic 绝不能杀死监控任务：任务一旦死亡，睡眠唤醒恢复、
            // 窗口同步、壁纸页僵死检测全部静默失效（tokio 任务 panic 无任何日志）。
            let tick = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                monitor_tick(
                    &app2,
                    &mut was_asleep,
                    &mut last_audio_seq,
                    &mut audio_stale_ticks,
                    &mut failed_retry_ticks,
                    &mut web_stall_ticks,
                )
            }));
            match tick {
                Ok(action) => {
                    if action.restart_audio {
                        // stop/start 含 ObjC 调用与最长数秒的等待，放在同步捕获
                        // 范围外的异步段执行
                        tracing::info!("restarting system audio capture");
                        let a = app2.clone();
                        let _ = audio_capture::stop(a.clone()).await;
                        let _ = audio_capture::start(a).await;
                        last_audio_seq = 0;
                    }
                    if action.reload_wallpapers {
                        tracing::warn!(
                            "web wallpaper page stalled (no sse clients); force reloading"
                        );
                        spawn_force_reload(app2.clone());
                    }
                }
                Err(p) => {
                    let msg = p
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".into());
                    tracing::error!("wallpaper monitor tick panicked: {msg}");
                }
            }
            if ticks % 30 == 0 {
                tracing::debug!(
                    "wallpaper monitor alive (ticks={ticks}, display_asleep={})",
                    platform::display_asleep()
                );
            }
        }
    });
}

/// 单次监控 tick（窗口同步 + 睡眠唤醒 + 音频看门狗 + web 壁纸活性检测）。
fn monitor_tick(
    app: &AppHandle,
    was_asleep: &mut bool,
    last_audio_seq: &mut u64,
    audio_stale_ticks: &mut u32,
    failed_retry_ticks: &mut u32,
    web_stall_ticks: &mut u32,
) -> TickActions {
    let mut action = TickActions::default();
    // 睡眠判定（复合信号，两道保险）：
    // ① CG 状态查询放在主线程（display 状态更新依赖运行循环，后台线程轮询
    //    会拿到卡死的陈旧值——实测合盖重开后进程内恒报 asleep=true）；
    // ② 音频样本恢复流动即证明系统实际已唤醒，覆盖 CG 谎报的情形。
    //    故有效睡眠 = 「主线程 CG 报告 && 样本未流动」。
    let fresh = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let app3 = app.clone();
        let (fresh_flag, done_flag) = (fresh.clone(), done.clone());
        let _ = app.run_on_main_thread(move || {
            let a = platform::display_asleep();
            ensure_windows_inner(&app3, a);
            fresh_flag.store(a, std::sync::atomic::Ordering::Relaxed);
            done_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        // 等主线程完成（通常 <10ms）；超时则退回用上一 tick 的状态。
        // 长时间不完成 = 主线程事件循环被卡死（软件无响应的直接信号），告警留痕。
        for _ in 0..50 {
            if done.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if !done.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::error!(
                "main thread did not process monitor dispatch within 100ms (UI event loop wedged?)"
            );
        }
    }
    let asleep_reported = fresh.load(std::sync::atomic::Ordering::Relaxed);
    let samples_flow = {
        let seq = app
            .try_state::<audio_capture::AudioCaptureState>()
            .map(|st| st.shared.snapshot().0)
            .unwrap_or(0);
        seq != *last_audio_seq
    };
    let asleep = asleep_reported && !samples_flow;
    if asleep && !*was_asleep {
        *was_asleep = true;
        if let Some(st) = app.try_state::<WallpaperEngineState>() {
            *st.paused.lock().unwrap() = true;
        }
        // 睡眠：释放壁纸窗口的渲染资源（画布/WebGL/视频/iframe），归还内存；醒来后由 restore() 重建
        eval_all(app, "window.__wp && window.__wp.release()");
        tracing::info!("display asleep: wallpapers released");
    } else if !asleep && *was_asleep {
        *was_asleep = false;
        if let Some(st) = app.try_state::<WallpaperEngineState>() {
            *st.paused.lock().unwrap() = false;
        }
        eval_all(app, "window.__wp && window.__wp.restore()");
        tracing::info!("display woke: wallpapers restored");
    }
    // 音频捕获健康看门狗（仅 RUNNING 相位参与）。
    let has_web = has_web_wallpaper(app);
    if let Some(st) = app.try_state::<audio_capture::AudioCaptureState>() {
        let shared = st.shared.clone();
        if shared.is_running() {
            let seq = shared.snapshot().0;
            if seq != *last_audio_seq {
                *last_audio_seq = seq;
                *audio_stale_ticks = 0;
            } else {
                *audio_stale_ticks += 1;
            }
            if *audio_stale_ticks >= 5 {
                *audio_stale_ticks = 0;
                if shared
                    .ever_received
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    *last_audio_seq = 0;
                    action.restart_audio = true;
                }
            }
        } else if shared.phase() == crate::audio_capture::PHASE_FAILED {
            // FAILED：本次启动失败（如合盖期间 SCStream 以「流播放无法启动音频」
            // 拒绝）。若此前曾成功工作过（权限必然已授予），每 ~30s 自动重试，
            // 开盖后自行恢复；从未成功过（无权限等永久性问题）不重试。
            *audio_stale_ticks = 0;
            if shared
                .ever_received
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                *failed_retry_ticks += 1;
                if *failed_retry_ticks >= 15 {
                    *failed_retry_ticks = 0;
                    action.restart_audio = true;
                }
            }
        } else {
            *audio_stale_ticks = 0;
            *failed_retry_ticks = 0;
        }
    }
    // web 壁纸页活性检测（与睡眠转换事件解耦）：休眠/合盖场景下 WebKit 页面可能
    // 被系统挂起后不再恢复（rAF/网络全停）。新 shim 下每个 web 壁纸页加载后必然
    // 持有 1 条 /audio-stream SSE 连接——capture 运行中若连续 3 个 tick（~6s）
    // 连接数为 0，判定页面僵死，用当前会话配置强制重载自愈。
    if has_web && action.restart_audio {
        // 本 tick 已要求重启音频：SSE 即将断开重连，跳过本轮活性判断
        *web_stall_ticks = 0;
    } else if has_web {
        let running = app
            .try_state::<audio_capture::AudioCaptureState>()
            .map(|s| s.shared.is_running())
            .unwrap_or(false);
        let sse = app
            .try_state::<ContentServerState>()
            .map(|st| st.sse_client_count())
            .unwrap_or(0);
        if running && sse == 0 {
            *web_stall_ticks += 1;
            if *web_stall_ticks >= 3 {
                *web_stall_ticks = 0;
                action.reload_wallpapers = true;
            }
        } else {
            *web_stall_ticks = 0;
        }
    } else {
        *web_stall_ticks = 0;
    }
    action
}

fn has_web_wallpaper(app: &AppHandle) -> bool {
    app.try_state::<WallpaperEngineState>()
        .map(|st| {
            st.windows
                .lock()
                .unwrap()
                .values()
                .any(|c| c.r#type == "web")
        })
        .unwrap_or(false)
}

/// 用当前会话配置强制重载所有 web 壁纸页（页面僵死自愈）。
fn spawn_force_reload(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let configs: Vec<(String, WallpaperConfig)> = app
            .try_state::<WallpaperEngineState>()
            .map(|st| st.windows.lock().unwrap().clone().into_iter().collect())
            .unwrap_or_default();
        for (label, mut cfg) in configs {
            if cfg.r#type != "web" {
                continue;
            }
            if let Some(w) = app.get_webview_window(&label) {
                cfg.media_base = media_base(&app);
                refresh_src(&app, &mut cfg);
                let item = item_id_of(&cfg);
                apply_play_config(&app, &mut cfg, item.as_deref());
                let query = config_query_with_audio(&cfg, content_token(&app).as_deref());
                if let Some(port) = app
                    .try_state::<Arc<Mutex<u16>>>()
                    .and_then(|p| p.lock().ok().map(|g| *g))
                    .filter(|p| *p > 0)
                {
                    let js = format!(
                        "window.location.replace('http://127.0.0.1:{port}/renderer/index.html{query}')"
                    );
                    let _ = w.eval(&js);
                }
            }
        }
    });
}

/// URL query 值编码
fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 渲染器 URL 的 query。`audio_token` 非空时渲染器会订阅 /audio-stream SSE，
/// 把系统音频频谱注入库（scene 与 web 壁纸共用同一份数据）。
fn config_query_with_audio(cfg: &WallpaperConfig, audio_token: Option<&str>) -> String {
    let mut parts = vec![format!("type={}", url_encode(&cfg.r#type))];
    if let Some(src) = &cfg.src {
        parts.push(format!("src={}", url_encode(src)));
    }
    parts.push(format!("fit={}", url_encode(&cfg.fit)));
    parts.push(format!("renderDpr={}", cfg.render_dpr));
    parts.push(format!("sceneFps={}", cfg.scene_fps));
    parts.push(format!("muted={}", cfg.muted));
    parts.push(format!("loop={}", cfg.r#loop));
    if let Some(base) = &cfg.media_base {
        // 渲染器与 PreviewModal 均读取 `mediaBase`，保持命名一致
        parts.push(format!("mediaBase={}", url_encode(base)));
    }
    if let Some(tok) = audio_token {
        parts.push(format!("audioToken={}", url_encode(tok)));
    }
    format!("?{}", parts.join("&"))
}

/// 内容服务器 token（渲染器订阅 /audio-stream 需要）
fn content_token(app: &AppHandle) -> Option<String> {
    app.try_state::<ContentServerState>()
        .map(|s| s.token.clone())
}

/// 显式应用新壁纸后，若当前暂停是「自动暂停」挂的则立即恢复播放。
///
/// 用户在设置界面点「应用」是在主动要求「给我看这张壁纸」；自动暂停只是
/// 切到后台时的临时状态，不该让新壁纸以暂停态挂载（看起来像壁纸坏了）。
/// 用户手动暂停不受影响（auto_paused=false 时不动）；轮播走 apply_item_inner，
/// 不触发本逻辑 —— 后台自动切换不该在用户看不见时恢复播放白烧 GPU。
fn resume_if_auto_paused(app: &AppHandle) {
    let Some(st) = app.try_state::<WallpaperEngineState>() else {
        return;
    };
    let was_auto = {
        let mut g = st.auto_paused.lock().unwrap();
        std::mem::replace(&mut *g, false)
    };
    if was_auto {
        let _ = resume_all(app.clone());
        tracing::info!("auto-pause: 应用新壁纸，恢复播放");
    }
}

// ---------- 命令 ----------

#[tauri::command(rename = "wallpaper_apply")]
pub fn apply(
    app: AppHandle,
    config: WallpaperConfig,
    display_id: Option<String>,
) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let app2 = app.clone();
    app.run_on_main_thread(move || {
        let res = apply_on_main(&app2, display_id, config, None);
        let _ = tx.send(res);
    })
    .map_err(|e| e.to_string())?;
    let res = rx.recv().map_err(|e| format!("壁纸引擎未响应: {e}"))?;
    if res.is_ok() {
        resume_if_auto_paused(&app);
    }
    res
}

#[tauri::command(rename = "wallpaper_stop")]
pub fn stop(app: AppHandle, display_id: Option<String>) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let app2 = app.clone();
    app.run_on_main_thread(move || {
        let state = match app2.try_state::<WallpaperEngineState>() {
            Some(s) => s,
            None => {
                let _ = tx.send(Err("引擎未就绪".into()));
                return;
            }
        };
        let labels: Vec<String> = match &display_id {
            Some(id) => vec![format!("wallpaper-{id}")],
            None => state.windows.lock().unwrap().keys().cloned().collect(),
        };
        let db = app2.try_state::<Arc<Mutex<rusqlite::Connection>>>();
        for label in &labels {
            if let Some(w) = app2.get_webview_window(label) {
                let _ = w.destroy();
            }
            state.windows.lock().unwrap().remove(label);
            if let Some(db) = &db {
                if let Ok(conn) = db.lock() {
                    let key = label.strip_prefix("wallpaper-").unwrap_or(label);
                    let _ = conn.execute(
                        "DELETE FROM wallpaper_sessions WHERE display_id = ?1",
                        [key],
                    );
                }
            }
        }
        let _ = tx.send(Ok(()));
    })
    .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| format!("壁纸引擎未响应: {e}"))?
}

#[tauri::command(rename = "wallpaper_list_sessions")]
pub fn list_sessions(_app: AppHandle, state: State<'_, WallpaperEngineState>) -> serde_json::Value {
    let windows = state.windows.lock().unwrap().clone();
    let paused = *state.paused.lock().unwrap();
    serde_json::json!({ "active": !windows.is_empty(), "paused": paused, "sessions": windows })
}

/// 当前已应用的本地库条目 id 集（供「本地库」页把已应用壁纸的应用按钮置为已应用/禁用）。
/// 读取 wallpaper_sessions 中非空 item_id；wallpaper_stop 会删除会话行，故已停止的不在此列，
/// 且重启后仍能反映「上次应用」的壁纸。
#[tauri::command(rename = "wallpaper_active_items")]
pub fn active_items(app: AppHandle) -> Result<Vec<String>, String> {
    let db = app
        .try_state::<Arc<Mutex<rusqlite::Connection>>>()
        .ok_or("DB 未就绪")?;
    let ids: Vec<String> = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT item_id FROM wallpaper_sessions
                 WHERE item_id IS NOT NULL AND item_id != ''",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    };
    // 过滤文件已丢失的条目：否则本地库会给一张已被删掉文件的壁纸打「已应用」徽章，
    // 且应用按钮被永久禁用 —— 用户既看不出问题，也没法重新应用。
    // 根目录读不到时（数据目录未就绪/权限缺失）不做过滤，避免误判成全部丢失。
    let root_ok = crate::library::wallpapers_dir(&app)
        .map(|d| d.is_dir())
        .unwrap_or(false);
    if !root_ok {
        return Ok(ids);
    }
    Ok(ids
        .into_iter()
        .filter(|id| {
            crate::library::item_dir(&app, id)
                .map(|d| crate::library::item_files_exist(&d))
                .unwrap_or(true)
        })
        .collect())
}

#[tauri::command(rename = "wallpaper_pause_all")]
pub fn pause_all(app: AppHandle) -> Result<(), String> {
    if let Some(st) = app.try_state::<WallpaperEngineState>() {
        *st.paused.lock().unwrap() = true;
    }
    eval_all(&app, "window.__wp && window.__wp.pause()");
    Ok(())
}

#[tauri::command(rename = "wallpaper_resume_all")]
pub fn resume_all(app: AppHandle) -> Result<(), String> {
    if let Some(st) = app.try_state::<WallpaperEngineState>() {
        *st.paused.lock().unwrap() = false;
    }
    eval_all(&app, "window.__wp && window.__wp.resume()");
    Ok(())
}

#[tauri::command(rename = "wallpaper_set_volume")]
pub fn set_volume(app: AppHandle, volume: f64) -> Result<(), String> {
    eval_all(
        &app,
        &format!("window.__wp && window.__wp.setVolume({volume})"),
    );
    Ok(())
}

#[tauri::command(rename = "wallpaper_set_fit")]
pub fn set_fit(app: AppHandle, fit: String) -> Result<(), String> {
    if !matches!(fit.as_str(), "cover" | "contain" | "stretch") {
        return Err(format!(
            "未知的显示模式: {fit}（可选 cover/contain/stretch）"
        ));
    }
    // 持久化为全局显示模式（下次应用/恢复壁纸时统一生效）
    if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = db::set_setting(&conn, "wallpaper_fit", &fit);
        }
    }
    let js = format!("window.__wp && window.__wp.setFit({:?})", fit);
    eval_all(&app, &js);
    Ok(())
}

/// 设置全局渲染分辨率上限（有效 dpr 封顶），持久化并对所有壁纸窗口实时生效。
#[tauri::command(rename = "wallpaper_set_render_dpr")]
pub fn set_render_dpr(app: AppHandle, dpr: f32) -> Result<(), String> {
    let dpr = dpr.clamp(RENDER_DPR_MIN, RENDER_DPR_MAX);
    if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = db::set_setting(&conn, "wallpaper_render_dpr", &format!("{dpr}"));
        }
    }
    tracing::info!("render dpr set: {dpr}");
    eval_all(
        &app,
        &format!("window.__wp && window.__wp.setRenderDpr({dpr})"),
    );
    Ok(())
}

/// 设置全局语言（壁纸 `language` 属性的默认值）。
///
/// 与清晰度/帧率不同，语言是**挂载时**经 properties 注入的（见
/// we_props::effective_props 与内容服务器 /props 端点），改完不实时下发，
/// 当前正在播放的壁纸要等下次应用才生效 —— 语言决定壁纸脚本分支，中途热切
/// 比重新挂载更容易出错。UI 侧应提示「重新应用壁纸后生效」。
#[tauri::command(rename = "wallpaper_set_language")]
pub fn set_language(app: AppHandle, language: String) -> Result<(), String> {
    if !crate::we_props::LANGUAGE_CHOICES.contains(&language.as_str()) {
        return Err(format!("不支持的语言: {language}"));
    }
    if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = db::set_setting(&conn, crate::we_props::LANGUAGE_SETTING_KEY, &language);
        }
    }
    tracing::info!("language set: {language}");
    Ok(())
}

/// 设置全局场景帧率上限（30/60/120），持久化并对所有壁纸窗口实时生效。
#[tauri::command(rename = "wallpaper_set_scene_fps")]
pub fn set_scene_fps(app: AppHandle, fps: u32) -> Result<(), String> {
    if !matches!(fps, 30 | 60 | 120) {
        return Err(format!("场景帧率仅支持 30/60/120（收到 {fps}）"));
    }
    if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = db::set_setting(&conn, "wallpaper_scene_fps", &fps.to_string());
        }
    }
    eval_all(
        &app,
        &format!("window.__wp && window.__wp.setSceneFps({fps})"),
    );
    Ok(())
}

/// 设置「隐藏图标」开关（桌面图标之下/壁纸上方 = 默认；开启后壁纸窗口在桌面图标之上，可接收鼠标/互动）。默认关闭。
#[tauri::command(rename = "wallpaper_interactive_set")]
pub fn interactive_set(app: AppHandle, enabled: bool) -> Result<(), String> {
    let db = app
        .try_state::<Arc<Mutex<rusqlite::Connection>>>()
        .ok_or("DB 未就绪")?;
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::set_setting(
            &conn,
            "wallpaper_interactive",
            if enabled { "true" } else { "false" },
        )?;
    }
    // 重新应用所有壁纸窗口的层级/鼠标行为（改设置即生效）
    let (tx, rx) = std::sync::mpsc::channel();
    let app2 = app.clone();
    let interactive = current_interactive(&app);
    pointer::set_interactive(interactive);
    app.run_on_main_thread(move || {
        let res = (|| -> Result<(), String> {
            let interactive = current_interactive(&app2);
            let screens = platform::active_screens();
            for s in &screens {
                let label = format!("wallpaper-{}", s.id);
                if let Some(w) = app2.get_webview_window(&label) {
                    platform::apply_desktop_window(&w, (s.x, s.y, s.w, s.h), interactive);
                }
            }
            Ok(())
        })();
        let _ = tx.send(res);
    })
    .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| format!("壁纸引擎未响应: {e}"))?
}

// ---------- 本地库条目应用 + 轮播（T3） ----------

/// project.json 声明的入口 HTML（相对壁纸根）。WE 用 `file` 字段指定入口，
/// 有些壁纸既无 web/index.html 也无根 index.html，只能靠它定位。
/// 拒绝绝对路径与 .. 穿越，且要求文件真实存在。
/// 注意：we_props::entry_dir_prefix 依赖同样的优先级来解析 file 属性的相对前缀，
/// 两处须保持一致，否则壁纸加载目录与其文件属性的基准目录会不一致。
pub(crate) fn project_json_entry(dir: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("project.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let rel = v.get("file")?.as_str()?.trim().replace('\\', "/");
    if rel.is_empty() || rel.starts_with('/') || rel.split('/').any(|seg| seg == "..") {
        return None;
    }
    if !dir.join(&rel).is_file() {
        return None;
    }
    Some(rel)
}

/// 查找壁纸目录里第一个 HTML 文件，返回相对路径。
/// 优先 web/ 子目录（WE 常规布局），其次根目录，最后深层子目录；同层按文件名排序保证稳定。
pub(crate) fn find_first_html(dir: &std::path::Path) -> Option<String> {
    fn walk(d: &std::path::Path, base: &std::path::Path, found: &mut Vec<(u8, String)>) {
        let Ok(entries) = std::fs::read_dir(d) else {
            return;
        };
        let mut names: Vec<_> = entries.flatten().collect();
        names.sort_by_key(|e| e.file_name());
        for e in names {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, found);
            } else if let Some(ext) = p.extension().and_then(|x| x.to_str()) {
                if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm") {
                    if let Ok(rel) = p.strip_prefix(base) {
                        let rel = rel.to_string_lossy().into_owned();
                        let prio = if rel.starts_with("web/") {
                            0u8
                        } else if !rel.contains('/') {
                            1u8
                        } else {
                            2u8
                        };
                        found.push((prio, rel));
                    }
                }
            }
        }
    }
    let mut found: Vec<(u8, String)> = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found.into_iter().next().map(|(_, rel)| rel)
}

/// 解析本地库壁纸文件 → 渲染器配置（src 指向内容服务器媒体 URL）
fn resolve_item_config(app: &AppHandle, item_id: &str) -> Result<WallpaperConfig, String> {
    let db = app
        .try_state::<Arc<Mutex<rusqlite::Connection>>>()
        .ok_or("DB 未就绪")?;
    let (wtype,) = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT type FROM library_items WHERE item_id = ?1",
            [item_id],
            |r| Ok((r.get::<_, String>(0)?,)),
        )
        .map_err(|_| "壁纸不在本地库中（请先下载）".to_string())?
    };
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("wallpapers")
        .join(item_id);
    // 文件被手工删除时，下面各类型分支只会报「未找到视频文件」「不支持的壁纸类型」
    // 之类的错，掩盖真实原因。这里先给准确诊断。
    if !crate::library::item_files_exist(&dir) {
        return Err(
            "壁纸文件已丢失（可能被手动删除）。请在本地库点「清理失效条目」后重新下载".into(),
        );
    }
    let media = media_base(app).ok_or("内容服务器未就绪")?;

    let find_first = |exts: &[&str]| -> Option<String> {
        let entries = std::fs::read_dir(&dir).ok()?;
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();
            if exts.iter().any(|x| lower.ends_with(x)) {
                return Some(format!("{media}/{item_id}/{name}"));
            }
        }
        None
    };

    let mut cfg = match wtype.as_str() {
        "video" => {
            let src = find_first(&[".mp4", ".webm", ".mov"]).ok_or("未找到视频文件")?;
            WallpaperConfig {
                r#type: "video".into(),
                src: Some(src),
                ..Default::default()
            }
        }
        "gif" => {
            let src = find_first(&[".gif"]).ok_or("未找到 GIF 文件")?;
            WallpaperConfig {
                r#type: "gif".into(),
                src: Some(src),
                ..Default::default()
            }
        }
        "web" => {
            let web = web_base(app).ok_or("内容服务器未就绪")?;
            let base = format!("{web}/{item_id}");
            // 优先级须与 we_props::entry_dir_prefix 一致
            let src = if let Some(rel) = project_json_entry(&dir) {
                // project.json 显式声明的入口（有些壁纸没有任何 index.html）
                format!("{base}/{rel}")
            } else if dir.join("web/index.html").is_file() {
                // WE 常规：主目录下 web/ 子目录
                format!("{base}/web/")
            } else if dir.join("index.html").is_file() {
                // 根目录 index.html
                format!("{base}/")
            } else if let Some(rel) = find_first_html(&dir) {
                // 未找到 index.html：回退到目录里第一个 HTML 文件
                tracing::info!("web wallpaper {item_id}: index.html 未找到，使用 {rel}");
                format!("{base}/{rel}")
            } else {
                return Err("未找到 index.html 或任何 HTML 文件".into());
            };
            WallpaperConfig {
                r#type: "web".into(),
                src: Some(src),
                ..Default::default()
            }
        }
        "scene" => {
            let has_pkg = dir.join("scenes/scene.pkg").is_file() || dir.join("scene.pkg").is_file();
            if !has_pkg {
                // 缺 scene.pkg：先用本地已有的依赖内容补齐；仍缺的自动加入下载队列
                let dep_ids = crate::download::read_project_json(&dir.join("project.json"))
                    .map(|v| crate::download::parse_dependency_ids(&v, item_id))
                    .unwrap_or_default();
                if dep_ids.is_empty() {
                    return Err("未找到 scene.pkg".into());
                }
                let Some(svc) = app.try_state::<Arc<crate::download::DownloadService>>() else {
                    return Err(format!(
                        "未找到 scene.pkg（该壁纸依赖工坊内容 {}）",
                        dep_ids.join("、")
                    ));
                };
                let missing = svc.settle_dependencies(&dir, &dep_ids);
                if !missing.is_empty() {
                    let queued = svc.enqueue_dependency_tasks(&missing, std::slice::from_ref(&dir));
                    if !queued.is_empty() {
                        return Err(format!(
                            "该壁纸依赖工坊内容 {}，已自动加入下载队列，下载完成后重新应用即可",
                            queued.join("、")
                        ));
                    }
                }
                let has_pkg =
                    dir.join("scenes/scene.pkg").is_file() || dir.join("scene.pkg").is_file();
                if !has_pkg {
                    return Err("未找到 scene.pkg（依赖内容已合并但仍缺入口文件）".into());
                }
            }
            WallpaperConfig {
                r#type: "scene".into(),
                src: Some(item_id.to_string()),
                ..Default::default()
            }
        }
        other => {
            if let Some(src) = find_first(&[".png", ".jpg", ".jpeg", ".webp"]) {
                WallpaperConfig {
                    r#type: "image".into(),
                    src: Some(src),
                    ..Default::default()
                }
            } else {
                return Err(format!("不支持的壁纸类型: {other}"));
            }
        }
    };
    // 统一补上媒体基址（scene 的 src 只是 itemId，渲染器需 mediaBase 拼 scene.pkg 地址）
    cfg.media_base = Some(media);
    Ok(cfg)
}

/// 把本地库条目应用到桌面（解析文件 → 全部显示器）。
/// 内部共用实现：不触碰播放/暂停状态（轮播在后台切换时必须保持自动暂停）。
fn apply_item_inner(app: &AppHandle, item_id: &str) -> Result<(), String> {
    let cfg = resolve_item_config(app, item_id)?;
    let (tx, rx) = std::sync::mpsc::channel();
    let app2 = app.clone();
    let item_id = item_id.to_string();
    app.run_on_main_thread(move || {
        let res = apply_on_main(&app2, None, cfg, Some(&item_id));
        let _ = tx.send(res);
    })
    .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| format!("壁纸引擎未响应: {e}"))?
}

/// 把本地库条目应用到桌面（用户显式点击；自动暂停态下立即恢复播放）
#[tauri::command(rename = "wallpaper_apply_item")]
pub fn apply_item(app: AppHandle, item_id: String) -> Result<(), String> {
    let res = apply_item_inner(&app, &item_id);
    if res.is_ok() {
        resume_if_auto_paused(&app);
    }
    res
}

/// 本地库条目预览信息（复用配置解析；前端按类型渲染弹框）
#[tauri::command(rename = "library_preview")]
pub fn library_preview(app: AppHandle, item_id: String) -> Result<WallpaperConfig, String> {
    resolve_item_config(&app, &item_id)
}

// ---------- 播放列表（轮播） ----------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub item_ids: Vec<String>,
    pub interval_sec: i64,
}

fn list_playlists(conn: &Connection) -> Result<Vec<Playlist>, String> {
    let mut stmt = conn
        .prepare("SELECT id, name, item_ids, interval_sec FROM playlists ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let raw: String = r.get(2)?;
            let item_ids: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
            Ok(Playlist {
                id: r.get(0)?,
                name: r.get(1)?,
                item_ids,
                interval_sec: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn playlist_list(app: AppHandle) -> Result<Vec<Playlist>, String> {
    let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    list_playlists(&conn)
}

#[tauri::command]
pub fn playlist_create(
    app: AppHandle,
    name: String,
    item_ids: Vec<String>,
    interval_sec: i64,
) -> Result<i64, String> {
    let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO playlists(name, item_ids, interval_sec) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            name,
            serde_json::to_string(&item_ids).unwrap_or_default(),
            interval_sec.max(30)
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

#[tauri::command]
pub fn playlist_delete(app: AppHandle, id: i64) -> Result<bool, String> {
    let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM playlists WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// 激活播放列表：设置 active_playlist 并立即应用第一项
#[tauri::command]
pub fn playlist_apply(app: AppHandle, id: i64) -> Result<serde_json::Value, String> {
    let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
    let playlist = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let all = list_playlists(&conn)?;
        all.into_iter()
            .find(|p| p.id == id)
            .ok_or("播放列表不存在")?
    };
    if playlist.item_ids.is_empty() {
        return Err("播放列表为空".into());
    }
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::set_setting(
            &conn,
            "active_playlist",
            &serde_json::to_string(&playlist).unwrap_or_default(),
        )?;
        db::set_setting(&conn, "playlist_index", "0")?;
    }
    // 应用第一项
    if let Some(first) = playlist.item_ids.first() {
        apply_item(app.clone(), first.clone())?;
    }
    tracing::info!(
        "playlist {} activated ({} items, {}s)",
        playlist.name,
        playlist.item_ids.len(),
        playlist.interval_sec
    );
    Ok(serde_json::to_value(&playlist).unwrap_or_default())
}

/// 下一张（手动或轮播定时）
#[tauri::command(rename = "wallpaper_next")]
pub fn next(app: AppHandle) -> Result<serde_json::Value, String> {
    let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
    let (playlist, index) = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let raw = db::get_setting(&conn, "active_playlist").ok_or("未激活播放列表")?;
        let p: Playlist = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let idx: i64 = db::get_setting(&conn, "playlist_index")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (p, idx)
    };
    if playlist.item_ids.is_empty() {
        return Err("播放列表为空".into());
    }
    let n = playlist.item_ids.len() as i64;
    let next_idx = (index + 1) % n;
    let item = playlist.item_ids[next_idx as usize].clone();
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::set_setting(&conn, "playlist_index", &next_idx.to_string())?;
    }
    apply_item_inner(&app, &item)?;
    Ok(serde_json::json!({ "itemId": item, "index": next_idx }))
}

/// 轮播定时任务：读取 active_playlist，按间隔自动下一张（由 init 启动）
pub fn start_playlist_rotation(app: &AppHandle) {
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut last_tick = chrono::Utc::now().timestamp();
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let now = chrono::Utc::now().timestamp();
            let interval = {
                let Some(db) = app2.try_state::<Arc<Mutex<rusqlite::Connection>>>() else {
                    continue;
                };
                let conn = match db.lock() {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                db::get_setting(&conn, "active_playlist")
                    .and_then(|raw| serde_json::from_str::<Playlist>(&raw).ok())
                    .map(|p| p.interval_sec.max(30))
                    .unwrap_or(0)
            };
            if interval > 0 && now - last_tick >= interval {
                last_tick = now;
                if let Err(e) = next(app2.clone()) {
                    tracing::debug!("playlist rotation: {e}");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play_cfg_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        c
    }

    /// 每壁纸播放设置的三态语义：缺失=跟随全局、有值=专属、全空=删键。
    ///
    /// 三态是这个特性的核心，也最容易写错成两态（"等于默认值就算跟随"）——
    /// 那样用户特意设成与全局相同的值，会在全局改动后被意外带走。
    #[test]
    fn item_play_config_three_state_roundtrip() {
        let c = play_cfg_db();
        // 无记录 → 全部跟随全局
        let empty = item_play_config(&c, "123");
        assert!(empty.fit.is_none() && empty.render_dpr.is_none());
        assert!(empty.scene_fps.is_none() && empty.volume.is_none());

        // 只覆盖一项，其余仍跟随
        let mut v = ItemPlayConfig::default();
        v.render_dpr = Some(2.0);
        set_item_play_config(&c, "123", &v).unwrap();
        let got = item_play_config(&c, "123");
        assert_eq!(got.render_dpr, Some(2.0));
        assert!(got.fit.is_none(), "未设置的项必须保持 None（跟随全局）");

        // 覆盖值与全局默认相同也算"专属"：不能因为值一样就当没设
        let mut same = ItemPlayConfig::default();
        same.scene_fps = Some(DEFAULT_SCENE_FPS);
        set_item_play_config(&c, "456", &same).unwrap();
        assert_eq!(
            item_play_config(&c, "456").scene_fps,
            Some(DEFAULT_SCENE_FPS)
        );

        // 全字段 None → 删键（而不是留一个空 JSON）
        set_item_play_config(&c, "123", &ItemPlayConfig::default()).unwrap();
        let left: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM settings WHERE key = 'play_cfg:123'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            left, 0,
            "回到全跟随时应删键，避免与'从未设置'在库里长得不一样"
        );

        // 壁纸之间互不影响
        assert_eq!(
            item_play_config(&c, "456").scene_fps,
            Some(DEFAULT_SCENE_FPS)
        );
    }

    /// item_id 反解：三种 src 形态 + 拿不到时返回 None（退化为只用全局）
    #[test]
    fn item_id_of_parses_all_src_shapes() {
        let mk = |t: &str, src: Option<&str>| WallpaperConfig {
            r#type: t.into(),
            src: src.map(|s| s.into()),
            ..Default::default()
        };
        // scene：src 就是 item_id
        assert_eq!(
            item_id_of(&mk("scene", Some("3781035191"))).as_deref(),
            Some("3781035191")
        );
        // web / 媒体：从 /web/<token>/<item>/... 里取第二段
        assert_eq!(
            item_id_of(&mk(
                "web",
                Some("http://127.0.0.1:1/web/tok/3406740580/index.html")
            ))
            .as_deref(),
            Some("3406740580")
        );
        assert_eq!(
            item_id_of(&mk("video", Some("http://127.0.0.1:1/media/tok/999/a.mp4"))).as_deref(),
            Some("999")
        );
        // 拿不到：外部 URL / 无 src / 相对路径
        assert_eq!(
            item_id_of(&mk("web", Some("https://example.com/x.html"))),
            None
        );
        assert_eq!(item_id_of(&mk("web", None)), None);
        assert_eq!(item_id_of(&mk("canvas", Some("/test-media/x"))), None);
    }

    fn fixture(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wpem-entry-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn project_json_entry_priority_and_safety() {
        // 声明的入口存在 → 采用（该壁纸没有任何 index.html）
        let d = fixture("declared");
        std::fs::write(d.join("bb.html"), "x").unwrap();
        std::fs::write(d.join("project.json"), r#"{"type":"web","file":"bb.html"}"#).unwrap();
        assert_eq!(project_json_entry(&d).as_deref(), Some("bb.html"));

        // 反斜杠归一化为正斜杠
        let d = fixture("backslash");
        std::fs::create_dir_all(d.join("pages")).unwrap();
        std::fs::write(d.join("pages/a.html"), "x").unwrap();
        std::fs::write(
            d.join("project.json"),
            r#"{"type":"web","file":"pages\\a.html"}"#,
        )
        .unwrap();
        assert_eq!(project_json_entry(&d).as_deref(), Some("pages/a.html"));

        // 声明的文件不存在 → None（交给后续常规探测）
        let d = fixture("missing");
        std::fs::write(
            d.join("project.json"),
            r#"{"type":"web","file":"nope.html"}"#,
        )
        .unwrap();
        assert_eq!(project_json_entry(&d), None);

        // 路径穿越与绝对路径一律拒绝
        for bad in [
            r#"{"file":"../../etc/passwd"}"#,
            r#"{"file":"/etc/passwd"}"#,
            r#"{"file":""}"#,
        ] {
            let d = fixture("unsafe");
            std::fs::write(d.join("project.json"), bad).unwrap();
            assert_eq!(project_json_entry(&d), None, "应拒绝: {bad}");
        }

        // 无 project.json / 无 file 字段
        let d = fixture("nofile");
        std::fs::write(d.join("project.json"), r#"{"type":"web"}"#).unwrap();
        assert_eq!(project_json_entry(&d), None);
        assert_eq!(project_json_entry(&fixture("empty")), None);
    }
}
