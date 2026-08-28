//! 壁纸引擎（T3）：多显示器桌面层窗口 + 会话持久化 + 睡眠暂停 + 轮播播放列表
//!
//! 每显示器一个 Tauri 桌面窗口（label = "wallpaper-<displayId>"），渲染器页
//! 经 URL query 注入配置；屏幕布局变化由后台监控任务同步（2s）；
//! 显示器睡眠由 CGDisplayIsAsleep 轮询（5s）驱动暂停/恢复。

pub mod macos;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use rusqlite::Connection;
use crate::db;

use crate::content_server::ContentServerState;

pub const DEFAULT_FIT: &str = "cover";
/// 渲染分辨率上限（有效 devicePixelRatio 的封顶）：越低越省内存（GPU 画布/纹理）。
/// 1.0 = 按逻辑分辨率渲染（Retina 上约为原先 1/4 内存）；2.0 = 不封顶（原清晰度）。
pub const DEFAULT_RENDER_DPR: f32 = 2.0;
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
        Some("contain") | Some("stretch") | Some("cover") => fit.unwrap_or_else(|| DEFAULT_FIT.into()),
        _ => DEFAULT_FIT.into(),
    }
}

/// 把全局显示模式写进配置（应用/恢复壁纸时统一以全局为准，实现"全局一个开关"）。
fn apply_global_fit(app: &AppHandle, cfg: &mut WallpaperConfig) {
    let fit: String;
    {
        let db = app.try_state::<Arc<Mutex<Connection>>>();
        fit = match db {
            Some(state) => match state.lock() {
                Ok(conn) => global_fit(Some(&conn)),
                Err(_) => DEFAULT_FIT.to_string(),
            },
            None => DEFAULT_FIT.to_string(),
        };
    }
    cfg.fit = fit;
}

/// 全局渲染分辨率上限（有效 dpr 封顶），读取设置 `wallpaper_render_dpr`，非法值回退到默认。
fn global_render_dpr(conn: Option<&Connection>) -> f32 {
    let raw = conn.and_then(|c| db::get_setting(c, "wallpaper_render_dpr"));
    let parsed = raw
        .as_deref()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .unwrap_or(DEFAULT_RENDER_DPR);
    parsed.clamp(0.5, 2.0)
}

/// 把全局渲染分辨率上限写进配置（应用/恢复壁纸时统一以全局为准）。
fn apply_global_render_dpr(app: &AppHandle, cfg: &mut WallpaperConfig) {
    let dpr: f32;
    {
        let db = app.try_state::<Arc<Mutex<Connection>>>();
        dpr = match db {
            Some(state) => match state.lock() {
                Ok(conn) => global_render_dpr(Some(&conn)),
                Err(_) => DEFAULT_RENDER_DPR,
            },
            None => DEFAULT_RENDER_DPR,
        };
    }
    cfg.render_dpr = dpr;
}

/// 全局场景帧率上限（读设置 `wallpaper_scene_fps`），只允许 30/60/120，非法值回退 60。
fn global_scene_fps(conn: Option<&Connection>) -> u32 {
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

/// 把全局场景帧率写进配置（应用/恢复壁纸时统一以全局为准）。
fn apply_global_scene_fps(app: &AppHandle, cfg: &mut WallpaperConfig) {
    let fps: u32;
    {
        let db = app.try_state::<Arc<Mutex<Connection>>>();
        fps = match db {
            Some(state) => match state.lock() {
                Ok(conn) => global_scene_fps(Some(&conn)),
                Err(_) => DEFAULT_SCENE_FPS,
            },
            None => DEFAULT_SCENE_FPS,
        };
    }
    cfg.scene_fps = fps;
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
}

impl Default for WallpaperEngineState {
    fn default() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            default: Mutex::new(None),
            paused: Mutex::new(false),
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
    ensure_windows(app);
    start_monitor(app);
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
        let rows = match stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
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
    // 显示器睡眠/唤醒切换期间不做任何窗口增删：此时 CGGetActiveDisplayList
    // 可能返回空列表（显示器从「活动」列表暂时消失），若照常执行下方清理逻辑，
    // 会把所有壁纸窗口误判为「已断开的显示器」全部销毁 —— 主窗口若也处于闲置
    // 释放状态，最后一个窗口关闭就会触发 Tauri 默认行为退出整个进程
    // （表现为「休眠后壁纸软件退出」）。醒来后由下一轮 tick 恢复同步。
    if macos::display_asleep() {
        return;
    }
    let screens = macos::active_screens();
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
                macos::set_frame(&win, *x, *y, *w, *h);
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
    let marker = if cfg.r#type == "web" { "/web/" } else { "/media/" };
    let Some(idx) = src.find(marker) else { return };
    let after = &src[idx + marker.len()..]; // <token>/<item_id>/<rest...>
    let Some(rest) = after.splitn(2, '/').nth(1) else { return }; // <item_id>/<rest...>
    let base = if cfg.r#type == "web" { web_base(app) } else { media_base(app) };
    let Some(base) = base else { return };
    cfg.src = Some(format!("{base}/{rest}"));
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
    apply_global_fit(app, &mut cfg);
    apply_global_render_dpr(app, &mut cfg);
    apply_global_scene_fps(app, &mut cfg);
    let query = config_query(&cfg);
    // 渲染器页与媒体同源（内容服务器），消除跨源 fetch 限制
    let port: u16 = match app.try_state::<Arc<Mutex<u16>>>() {
        Some(s) => match s.lock() {
            Ok(g) => *g,
            Err(_) => 0,
        },
        None => 0,
    };
    let url = if port > 0 {
        let parsed: url::Url =
            format!("http://127.0.0.1:{port}/renderer/index.html{query}")
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

    #[cfg(target_os = "macos")]
    macos::apply_desktop_window(&window, frame, current_interactive(app));

    window.show().map_err(|e| e.to_string())?;
    tracing::info!("wallpaper window {label} created: {cfg:?}");
    Ok(window)
}

fn apply_on_main(
    app: &AppHandle,
    display_id: Option<String>,
    cfg: WallpaperConfig,
    item_id: Option<&str>,
) -> Result<(), String> {
    let screens = macos::active_screens();
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

    let state = app.try_state::<WallpaperEngineState>().ok_or("引擎未就绪")?;
    let db = app.try_state::<Arc<Mutex<rusqlite::Connection>>>().ok_or("DB 未就绪")?;

    for (label, frame) in &targets {
        let window = match app.get_webview_window(label) {
            Some(w) => w,
            None => create_desktop_window(app, label, &cfg, *frame).map_err(|e| e.to_string())?,
        };
        #[cfg(target_os = "macos")]
        macos::apply_desktop_window(&window, *frame, current_interactive(app));

        let mut cfg2 = cfg.clone();
        cfg2.media_base = media_base(app);
        apply_global_fit(app, &mut cfg2);
        apply_global_render_dpr(app, &mut cfg2);
        apply_global_scene_fps(app, &mut cfg2);
        let js = format!(
            "window.__wp && window.__wp.setWallpaper({})",
            serde_json::to_string(&cfg2).map_err(|e| e.to_string())?
        );
        window.eval(&js).map_err(|e| e.to_string())?;
        state.windows.lock().unwrap().insert(label.clone(), cfg2.clone());

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

/// 是否开启「交互壁纸」（壁纸窗口在桌面图标之上并接收鼠标）；默认关闭
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

fn start_monitor(app: &AppHandle) {
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut was_asleep = false;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            // AppKit 窗口操作必须在主线程
            let app3 = app2.clone();
            let _ = app2.run_on_main_thread(move || {
                ensure_windows(&app3);
            });
            // 睡眠检测（轮询）
            let now = chrono::Utc::now().timestamp();
            {
                let asleep = macos::display_asleep();
                if asleep && !was_asleep {
                    was_asleep = true;
                    if let Some(st) = app2.try_state::<WallpaperEngineState>() {
                        *st.paused.lock().unwrap() = true;
                    }
                    // 睡眠：释放壁纸窗口的渲染资源（画布/WebGL/视频/iframe），归还内存；醒来后由 restore() 重建
                    eval_all(&app2, "window.__wp && window.__wp.release()");
                    tracing::info!("display asleep: wallpapers released");
                } else if !asleep && was_asleep {
                    was_asleep = false;
                    if let Some(st) = app2.try_state::<WallpaperEngineState>() {
                        *st.paused.lock().unwrap() = false;
                    }
                    eval_all(&app2, "window.__wp && window.__wp.restore()");
                    tracing::info!("display woke: wallpapers restored");
                }
            }
            let _ = now;
        }
    });
}

/// URL query 值编码
fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn config_query(cfg: &WallpaperConfig) -> String {
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
    format!("?{}", parts.join("&"))
}

// ---------- 命令 ----------

#[tauri::command(rename = "wallpaper_apply")]
pub fn apply(app: AppHandle, config: WallpaperConfig, display_id: Option<String>) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let app2 = app.clone();
    app.run_on_main_thread(move || {
        let res = apply_on_main(&app2, display_id, config, None);
        let _ = tx.send(res);
    })
    .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| format!("壁纸引擎未响应: {e}"))?
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
                    let _ = conn.execute("DELETE FROM wallpaper_sessions WHERE display_id = ?1", [key]);
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
    let db = app.try_state::<Arc<Mutex<rusqlite::Connection>>>().ok_or("DB 未就绪")?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT item_id FROM wallpaper_sessions
             WHERE item_id IS NOT NULL AND item_id != ''",
        )
        .map_err(|e| e.to_string())?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    Ok(ids)
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
    eval_all(&app, &format!("window.__wp && window.__wp.setVolume({volume})"));
    Ok(())
}

#[tauri::command(rename = "wallpaper_set_fit")]
pub fn set_fit(app: AppHandle, fit: String) -> Result<(), String> {
    if !matches!(fit.as_str(), "cover" | "contain" | "stretch") {
        return Err(format!("未知的显示模式: {fit}（可选 cover/contain/stretch）"));
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
    let dpr = dpr.clamp(0.5, 2.0);
    if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = db::set_setting(&conn, "wallpaper_render_dpr", &format!("{dpr}"));
        }
    }
    eval_all(&app, &format!("window.__wp && window.__wp.setRenderDpr({dpr})"));
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
    eval_all(&app, &format!("window.__wp && window.__wp.setSceneFps({fps})"));
    Ok(())
}

/// 设置「交互壁纸」开关（壁纸窗口在桌面图标之上，可接收鼠标/互动）。默认关闭。
#[tauri::command(rename = "wallpaper_interactive_set")]
pub fn interactive_set(app: AppHandle, enabled: bool) -> Result<(), String> {
    let db = app.try_state::<Arc<Mutex<rusqlite::Connection>>>().ok_or("DB 未就绪")?;
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
    app.run_on_main_thread(move || {
        let res = (|| -> Result<(), String> {
            let interactive = current_interactive(&app2);
            let screens = macos::active_screens();
            for s in &screens {
                let label = format!("wallpaper-{}", s.id);
                if let Some(w) = app2.get_webview_window(&label) {
                    macos::apply_desktop_window(&w, (s.x, s.y, s.w, s.h), interactive);
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

/// 查找壁纸目录里第一个 HTML 文件，返回相对路径。
/// 优先 web/ 子目录（WE 常规布局），其次根目录，最后深层子目录；同层按文件名排序保证稳定。
fn find_first_html(dir: &std::path::Path) -> Option<String> {
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
    let db = app.try_state::<Arc<Mutex<rusqlite::Connection>>>().ok_or("DB 未就绪")?;
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
            let src = find_first(&[".mp4", ".webm", ".mov"])
                .ok_or("未找到视频文件")?;
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
            let src = if dir.join("web/index.html").is_file() {
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
            let has_pkg = dir.join("scenes/scene.pkg").is_file()
                || dir.join("scene.pkg").is_file();
            if !has_pkg {
                // 有依赖声明但本地缺 scene.pkg：提示重新下载（会自动拉取依赖）
                let dep = std::fs::read_to_string(dir.join("project.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|v| v.get("dependency").and_then(|d| d.as_str()).map(|s| s.to_string()))
                    .filter(|d| !d.is_empty());
                if let Some(dep_id) = dep {
                    return Err(format!(
                        "未找到 scene.pkg（该壁纸依赖工坊物品 {dep_id}）。请到「下载」页删除并重新下载本壁纸，会自动拉取依赖。"
                    ));
                }
                return Err("未找到 scene.pkg".into());
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

/// 把本地库条目应用到桌面（解析文件 → 全部显示器）
#[tauri::command(rename = "wallpaper_apply_item")]
pub fn apply_item(app: AppHandle, item_id: String) -> Result<(), String> {
    let cfg = resolve_item_config(&app, &item_id)?;
    let (tx, rx) = std::sync::mpsc::channel();
    let app2 = app.clone();
    app.run_on_main_thread(move || {
        let res = apply_on_main(&app2, None, cfg, Some(&item_id));
        let _ = tx.send(res);
    })
    .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| format!("壁纸引擎未响应: {e}"))?
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
            let item_ids: Vec<String> =
                serde_json::from_str(&raw).unwrap_or_default();
            Ok(Playlist {
                id: r.get(0)?,
                name: r.get(1)?,
                item_ids,
                interval_sec: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
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
        rusqlite::params![name, serde_json::to_string(&item_ids).unwrap_or_default(), interval_sec.max(30)],
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
        all.into_iter().find(|p| p.id == id).ok_or("播放列表不存在")?
    };
    if playlist.item_ids.is_empty() {
        return Err("播放列表为空".into());
    }
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::set_setting(&conn, "active_playlist", &serde_json::to_string(&playlist).unwrap_or_default())?;
        db::set_setting(&conn, "playlist_index", "0")?;
    }
    // 应用第一项
    if let Some(first) = playlist.item_ids.first() {
        apply_item(app.clone(), first.clone())?;
    }
    tracing::info!("playlist {} activated ({} items, {}s)", playlist.name, playlist.item_ids.len(), playlist.interval_sec);
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
    apply_item(app.clone(), item.clone())?;
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
