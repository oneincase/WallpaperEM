//! 壁纸引擎（T3）：多显示器桌面层窗口 + 会话持久化 + 睡眠暂停 + 轮播播放列表
//!
//! 每显示器一个 Tauri 桌面窗口（label = "wallpaper-<displayId>"），渲染器页
//! 经 URL query 注入配置；屏幕布局变化由后台监控任务同步（2s）；
//! 显示器睡眠由 CGDisplayIsAsleep 轮询（5s）驱动暂停/恢复。

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;
pub mod platform;
pub mod pointer;
pub mod wheel;

use crate::audio_capture;
use crate::db;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_desktop_underlay::DesktopUnderlayExt;

use crate::content_server::ContentServerState;

pub const DEFAULT_FIT: &str = "cover";
/// 清晰度（相对设备 devicePixelRatio 的倍率）：越低越省内存（GPU 画布/纹理）。
/// 四档：0 自动（=设备 DPR，Retina 原生）/ 0.75 省电 / 0.85 标准 / 1 高清（=原生）。
///
/// 这里存的是**相对倍率**，壁纸页 renderer 收到后乘 `devicePixelRatio` 换算成
/// webwallgl 库使用的绝对 DPR（库 1.3.22+：0=自动，正数=目标 DPR，可高于设备上报值，
/// 解决宿主 WKWebView 把 devicePixelRatio 报成 1 时高清档被钉在逻辑像素的问题）。
/// 值域 [0,1]：倍率超过 1 只会超采样、无清晰度收益且费显存，故封顶 1。
pub const RENDER_DPR_MIN: f32 = 0.0;
pub const RENDER_DPR_MAX: f32 = 1.0;
/// 0 = 自动（跟随设备像素比，默认）。
pub const DEFAULT_RENDER_DPR: f32 = 0.0;
/// 场景壁纸帧率上限（帧/秒）：越低 GPU 占用越低。
/// 可选 15 / 24 / 30 / 45 / 60 / 120，默认 24（低功耗，多数场景 24fps 观感足够）。
pub const DEFAULT_SCENE_FPS: u32 = 24;
/// 允许的全局帧率档位（托盘、设置页、`wallpaper_set_scene_fps` 共用同一份白名单）
pub const SCENE_FPS_CHOICES: [u32; 6] = [15, 24, 30, 45, 60, 120];

/// 抗锯齿模式（webwallgl 库 1.3.23+）：off 默认（=库旧行为）/ fxaa 帧末后处理
/// （全画面边缘）/ msaa2 / msaa4 多重采样（只平滑几何边缘）。
pub const DEFAULT_AA: &str = "off";
pub const AA_CHOICES: [&str; 4] = ["off", "fxaa", "msaa2", "msaa4"];
/// 粒子质量档：high 默认 / medium / low（按倍率同缩数量上限与发射率）/ off（不渲染不推进）。
pub const DEFAULT_PARTICLES: &str = "high";
pub const PARTICLE_QUALITY_CHOICES: [&str; 4] = ["off", "low", "medium", "high"];
/// 后处理质量档：high 默认 / medium / low（压效果链 FBO 分辨率）。
/// 不提供 off：整屏关后处理会让辉光/水波类壁纸直接失去画面效果（v1.0.1 起
/// 从档位里移除；历史存量为 off 的读取时归一到 low）。
pub const DEFAULT_POST: &str = "high";
pub const POST_QUALITY_CHOICES: [&str; 3] = ["low", "medium", "high"];

/// 读取后处理档时的归一化：旧版本允许存 off，现归一到 low（不再有关档）
fn normalize_post(v: Option<String>) -> String {
    match v.as_deref() {
        Some("off") => "low".into(),
        Some(x) if POST_QUALITY_CHOICES.contains(&x) => v.unwrap(),
        _ => DEFAULT_POST.into(),
    }
}

/// 全局滤镜（帧率上限下面的那组）。**id 白名单本身就是契约**：
/// 托盘菜单、设置项、URL query、`__wp.setFilter` 传的都只是这份 id，
/// CSS filter 表达式只存在于渲染器里 —— 不让任意字符串流进 `style.filter`
/// 是刻意的（`url()` 能外链资源），也让宿主与渲染器共用同一套取值。
/// 顺序 = 托盘菜单顺序；`none` 必须存在且为默认。
/// 中文名与上游独立测试台（webwallgl bench）的 i18n 文案保持一致，
/// 两边调同一个效果时看到的是同一个词。
pub const WALLPAPER_FILTERS: &[(&str, &str)] = &[
    ("none", "无"),
    ("blur", "高斯模糊"),
    ("grayscale", "黑白"),
    ("sepia", "怀旧"),
    ("vivid", "鲜艳"),
    ("warm", "暖色"),
    ("cool", "冷色"),
    ("invert", "反色"),
    ("brighten", "提亮"),
    ("darken", "压暗"),
    ("contrast", "高对比"),
];
/// 默认滤镜 id（= 不套任何 CSS filter）
pub const DEFAULT_FILTER: &str = "none";

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
fn default_filter() -> String {
    DEFAULT_FILTER.into()
}
fn default_aa() -> String {
    DEFAULT_AA.into()
}
fn default_particles() -> String {
    DEFAULT_PARTICLES.into()
}
fn default_post() -> String {
    DEFAULT_POST.into()
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

/// 全局场景帧率上限（读设置 `wallpaper_scene_fps`），只允许 [`SCENE_FPS_CHOICES`]，
/// 非法值回退默认。
pub(crate) fn global_scene_fps(conn: Option<&Connection>) -> u32 {
    let raw = conn.and_then(|c| db::get_setting(c, "wallpaper_scene_fps"));
    let parsed = raw
        .as_deref()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_SCENE_FPS);
    if SCENE_FPS_CHOICES.contains(&parsed) {
        parsed
    } else {
        DEFAULT_SCENE_FPS
    }
}

/// 全局滤镜 id（读设置 `wallpaper_filter`）。不在白名单内（旧值/手改 DB）回退默认。
///
/// 作用范围是**桌面壁纸窗口**（label = wallpaper-*）：托盘菜单的下发走
/// `eval_all`，只遍历这批窗口；本地库预览是主窗口里的 iframe、走独立配置，
/// 不读这个设置 —— 这也是需求里「壁纸预览除外」的落地方式。
pub(crate) fn global_filter(conn: Option<&Connection>) -> String {
    let raw = conn.and_then(|c| db::get_setting(c, "wallpaper_filter"));
    match raw.as_deref() {
        Some(id) if WALLPAPER_FILTERS.iter().any(|(k, _)| *k == id) => id.to_string(),
        _ => DEFAULT_FILTER.into(),
    }
}

/// 全局抗锯齿模式（读设置 `wallpaper_aa`），白名单外回退默认 off。
fn global_aa(conn: Option<&Connection>) -> String {
    let raw = conn.and_then(|c| db::get_setting(c, "wallpaper_aa"));
    match raw.as_deref() {
        Some(v) if AA_CHOICES.contains(&v) => v.to_string(),
        _ => DEFAULT_AA.into(),
    }
}

/// 全局粒子质量档（读设置 `wallpaper_particles`），白名单外回退默认 high。
fn global_particles(conn: Option<&Connection>) -> String {
    let raw = conn.and_then(|c| db::get_setting(c, "wallpaper_particles"));
    match raw.as_deref() {
        Some(v) if PARTICLE_QUALITY_CHOICES.contains(&v) => v.to_string(),
        _ => DEFAULT_PARTICLES.into(),
    }
}

/// 全局后处理质量档（读设置 `wallpaper_post`），白名单外回退默认 high；
/// 旧版本存过 off 的归一到 low（off 档已移除，后处理不再允许整屏关闭）。
fn global_post(conn: Option<&Connection>) -> String {
    normalize_post(conn.and_then(|c| db::get_setting(c, "wallpaper_post")))
}

/// WE 官方素材通路开关（设置 `wallpaper_local_assets`）。
/// 默认开启：本机装了 Wallpaper Engine 就用官方原版像素（效果链/粒子/渐变/字体），
/// 没装时内容服务器探测返回 ok:false、库只多一次轻量请求并回落到程序化复刻。
fn global_local_assets(conn: Option<&Connection>) -> bool {
    conn.and_then(|c| db::get_setting(c, "wallpaper_local_assets"))
        .map(|v| v == "true" || v == "1")
        .unwrap_or(true)
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
    /// 抗锯齿（off/fxaa/msaa2/msaa4，库 1.3.23+）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aa: Option<String>,
    /// 粒子质量（off/low/medium/high，库 1.3.23+）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub particles: Option<String>,
    /// 后处理质量（off/low/medium/high，库 1.3.23+）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_processing: Option<String>,
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
    let empty = v.fit.is_none()
        && v.render_dpr.is_none()
        && v.scene_fps.is_none()
        && v.volume.is_none()
        && v.aa.is_none()
        && v.particles.is_none()
        && v.post_processing.is_none();
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
        cfg.filter = DEFAULT_FILTER.into();
        cfg.aa = DEFAULT_AA.into();
        cfg.particles = DEFAULT_PARTICLES.into();
        cfg.post_processing = DEFAULT_POST.into();
        cfg.local_assets = true;
        return;
    };
    let Ok(conn) = state.lock() else {
        cfg.fit = DEFAULT_FIT.into();
        cfg.render_dpr = DEFAULT_RENDER_DPR;
        cfg.scene_fps = DEFAULT_SCENE_FPS;
        cfg.filter = DEFAULT_FILTER.into();
        cfg.aa = DEFAULT_AA.into();
        cfg.particles = DEFAULT_PARTICLES.into();
        cfg.post_processing = DEFAULT_POST.into();
        cfg.local_assets = true;
        return;
    };
    // 先铺全局
    cfg.fit = global_fit(Some(&conn));
    cfg.render_dpr = global_render_dpr(Some(&conn));
    cfg.scene_fps = global_scene_fps(Some(&conn));
    cfg.filter = global_filter(Some(&conn));
    cfg.aa = global_aa(Some(&conn));
    cfg.particles = global_particles(Some(&conn));
    cfg.post_processing = global_post(Some(&conn));
    cfg.local_assets = global_local_assets(Some(&conn));
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
            if SCENE_FPS_CHOICES.contains(&f) {
                cfg.scene_fps = f;
            }
        }
        if let Some(v) = ov.volume {
            // 精确音量随配置下发：渲染器挂载时先静音，加载完成后按它起音量；
            // muted 是旧布尔近似，同步保持一致（老渲染器/旧会话仍读它）
            let v = v.clamp(0.0, 1.0);
            cfg.volume = Some(v);
            cfg.muted = v <= 0.0;
        }
        if let Some(v) = ov.aa.as_deref() {
            if AA_CHOICES.contains(&v) {
                cfg.aa = v.to_string();
            }
        }
        if let Some(v) = ov.particles.as_deref() {
            if PARTICLE_QUALITY_CHOICES.contains(&v) {
                cfg.particles = v.to_string();
            }
        }
        if let Some(v) = ov.post_processing.as_deref() {
            // 旧数据可能存过 off（该档已移除）：归一到 low
            let v = if v == "off" { "low" } else { v };
            if POST_QUALITY_CHOICES.contains(&v) {
                cfg.post_processing = v.to_string();
            }
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
            "aa": global_aa(Some(&conn)),
            "particles": global_particles(Some(&conn)),
            "postProcessing": global_post(Some(&conn)),
        }
    }))
}

/// 播放相关字段重置为库默认（下一步重新合成「全局默认 + 本壁纸覆盖」用）。
/// 播放覆盖变更后刷新正在使用的配置（state.windows 与持久化会话）时用：
/// 直接在旧配置上叠加会让被删掉的覆盖值（如音量）一直留在会话里，重启后复活。
fn reset_play_fields(cfg: &mut WallpaperConfig) {
    cfg.fit = DEFAULT_FIT.into();
    cfg.render_dpr = DEFAULT_RENDER_DPR;
    cfg.scene_fps = DEFAULT_SCENE_FPS;
    cfg.filter = DEFAULT_FILTER.into();
    cfg.aa = DEFAULT_AA.into();
    cfg.particles = DEFAULT_PARTICLES.into();
    cfg.post_processing = DEFAULT_POST.into();
    cfg.local_assets = true;
    cfg.volume = None;
    cfg.muted = default_muted();
}

/// 配置的生效音量（下发渲染器用）：精确值优先，缺省回落 muted 布尔。
fn effective_volume(cfg: &WallpaperConfig) -> f32 {
    cfg.volume
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(if cfg.muted { 0.0 } else { 1.0 })
}

/// 写某壁纸的播放设置并立即生效（该壁纸正在播放时才下发）
#[tauri::command(async, rename = "wallpaper_item_play_config_set")]
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
    // 只有这张壁纸正在某个屏幕上播放时才需要即时下发；否则下次应用时自然生效。
    // 按 item 过滤窗口而不是 eval_all：多显示器各放各的壁纸时，把 A 的音量
    // 下发给 B 会改掉 B 的生效值。
    let Some(state) = app.try_state::<WallpaperEngineState>() else {
        return Ok(());
    };
    let db = app.try_state::<Arc<Mutex<Connection>>>();
    let labels: Vec<String> = {
        let windows = state.windows.lock().unwrap();
        windows
            .iter()
            .filter(|(_, c)| item_id_of(c).as_deref() == Some(item_id.as_str()))
            .map(|(l, _)| l.clone())
            .collect()
    };
    if labels.is_empty() {
        return Ok(());
    }
    for label in labels {
        // 重算这块窗口的生效配置（全局默认 + 刚写入的覆盖）：
        // 「恢复全局设置」时覆盖字段全是 None，旧实现只下发 Some 字段 → 什么都不
        // 发 → 渲染器还留着被删掉的音量，壁纸继续出声。改为把生效值整个下发。
        let mut cfg2 = {
            let windows = state.windows.lock().unwrap();
            match windows.get(&label) {
                Some(c) => c.clone(),
                None => continue,
            }
        };
        let display_id_key = label.strip_prefix("wallpaper-").unwrap_or(&label).to_string();
        reset_play_fields(&mut cfg2);
        apply_play_config(&app, &mut cfg2, Some(&item_id));
        for w in wallpaper_windows(&app, &label) {
            let _ = w.eval(&format!(
                "window.__wp && window.__wp.setFit({})",
                serde_json::json!(cfg2.fit)
            ));
            let _ = w.eval(&format!(
                "window.__wp && window.__wp.setRenderDpr({})",
                cfg2.render_dpr
            ));
            let _ = w.eval(&format!(
                "window.__wp && window.__wp.setSceneFps({})",
                cfg2.scene_fps
            ));
            let _ = w.eval(&format!(
                "window.__wp && window.__wp.setVolume({})",
                effective_volume(&cfg2)
            ));
            let _ = w.eval(&format!(
                "window.__wp && window.__wp.setQuality({{antiAliasing:{}}})",
                serde_json::json!(cfg2.aa)
            ));
            let _ = w.eval(&format!(
                "window.__wp && window.__wp.setQuality({{particles:{}}})",
                serde_json::json!(cfg2.particles)
            ));
            let _ = w.eval(&format!(
                "window.__wp && window.__wp.setQuality({{postProcessing:{}}})",
                serde_json::json!(cfg2.post_processing)
            ));
        }
        // 同步内存态与持久化会话：否则 state.windows/会话里的旧覆盖值（volume/
        // muted 等）要等下次「应用」才被冲掉，期间重启会带着旧音量回来
        state.windows.lock().unwrap().insert(label.clone(), cfg2.clone());
        if let Some(db) = &db {
            if let Ok(conn) = db.lock() {
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
    /// 场景壁纸帧率上限（15/24/30/45/60/120），越低 GPU 占用越低
    #[serde(default = "default_scene_fps")]
    pub scene_fps: u32,
    /// 全局滤镜 id（见 WALLPAPER_FILTERS 白名单）
    #[serde(default = "default_filter")]
    pub filter: String,
    /// 抗锯齿模式（off/fxaa/msaa2/msaa4，库 1.3.23+）
    #[serde(default = "default_aa")]
    pub aa: String,
    /// 粒子质量档（off/low/medium/high，库 1.3.23+）
    #[serde(default = "default_particles")]
    pub particles: String,
    /// 后处理质量档（off/low/medium/high，库 1.3.23+）
    #[serde(default = "default_post")]
    pub post_processing: String,
    #[serde(default = "default_muted")]
    pub muted: bool,
    /// 音量 0..1 的精确值（0 即静音）。挂载时一律先静音，渲染器在壁纸完全
    /// 加载完成后再按这个值统一起音量（muted 只是旧字段的布尔近似，保留兼容）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    #[serde(default = "default_loop")]
    pub r#loop: bool,
    /// 场景壁纸的 scene.pkg 相对路径（project.json `file` 声明且非默认布局时）；
    /// 渲染器优先按它拉取，找不到再走默认的 scene.pkg / scenes/scene.pkg 回退链
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_pkg: Option<String>,
    /// 启用 WE 官方素材通路（渲染器 URL 带 ?localAssets=1，库 1.4.1+）：本机安装
    /// Wallpaper Engine 时，效果链/粒子/渐变/文字字体引用官方原版像素；未安装或
    /// 关闭时库静默回落到内置程序化复刻。挂载时生效，切换后需重新应用壁纸。
    #[serde(default)]
    pub local_assets: bool,
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
            filter: default_filter(),
            aa: default_aa(),
            particles: default_particles(),
            post_processing: default_post(),
            muted: default_muted(),
            volume: None,
            r#loop: default_loop(),
            scene_pkg: None,
            local_assets: false,
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
    /// 「暂停释放内存」已把壁纸渲染器整个销毁（自动暂停的加强形态）。
    ///
    /// 置位期间壁纸窗口不存在、但 `windows` 里的会话配置**保留** ——
    /// `ensure_windows` 见此标志不建窗（否则 2s 一轮的监控会把刚杀掉的
    /// 渲染进程立刻建回来），恢复播放时清标志并按配置整窗重建
    /// （[`resume_all`]）。这是比 `__wp.release()`（页面内 JS 释放）更彻底的
    /// 一档：进程直接结束，内存实打实归还。
    pub released: Mutex<bool>,
    /// macOS：label -> 该窗口**独占**的 WKWebsiteDataStore 标识。
    ///
    /// 窗口被销毁（stop / 显示器移除 / 换纸降级到重建）时按它
    /// `removeDataStoreForIdentifier`，删成功则这份存储的进程池被销毁，压着整张
    /// 壁纸的 WebContent 进程随之退出（见 [`destroy_wallpaper_window`]）。
    /// 删成功**不是必然**：只要 UI 进程里还活着一个引用该存储的 `WKWebsiteDataStore`，
    /// WebKit 就拒绝删除（实测 macOS 26 约七成如此），所以日常换壁纸走的是
    /// 「同窗口换文档」（[`schedule_window_reload`]），不依赖这条。
    /// 其它平台不用（WebView2 / WebKitGTK 的 destroy 会连带销毁渲染进程）。
    #[cfg(target_os = "macos")]
    pub data_stores: Mutex<HashMap<String, [u8; 16]>>,
}

impl Default for WallpaperEngineState {
    fn default() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            default: Mutex::new(None),
            paused: Mutex::new(false),
            auto_paused: Mutex::new(false),
            released: Mutex::new(false),
            #[cfg(target_os = "macos")]
            data_stores: Mutex::new(HashMap::new()),
        }
    }
}

/// 初始化：管理状态 + 恢复会话 + 启动监控任务（屏幕布局 2s / 睡眠 5s）
pub fn init(app: &AppHandle) -> tauri::Result<()> {
    app.manage(WallpaperEngineState::default());
    // 显示器名称缓存 + 稳定 id 迁移都要先于会话恢复（setup 跑在主线程，NSScreen 可用）：
    // 迁移会把旧 CGDirectDisplayID 键改写成稳定 id，晚于恢复就对不上窗口 label 了
    platform::refresh_display_meta();
    migrate_display_ids(app);
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
    // 启动瞬间显示器枚举可能是空的（桌面/显示驱动还在就绪，Windows 实测首帧返回空），
    // 这一轮建窗就会空转，只能干等监控的 2s tick —— 表现为「启动后要过两秒壁纸才出现」。
    // 这里补一段有界短重试：一旦枚举到显示器立刻建窗，把首帧提前到 ~0.3s。
    if platform::active_screens().is_empty() {
        tracing::info!("启动时显示器枚举为空，进入短重试建窗");
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let (tx, rx) = std::sync::mpsc::channel();
                let a = app2.clone();
                let _ = app2.run_on_main_thread(move || {
                    let has_screens = !platform::active_screens().is_empty();
                    if has_screens {
                        ensure_windows(&a);
                    }
                    let _ = tx.send(has_screens);
                });
                if matches!(rx.recv_timeout(Duration::from_secs(2)), Ok(true)) {
                    break;
                }
            }
        });
    }
    start_monitor(app);
    // 自动暂停（默认关）：监听前台应用切换，切到非桌面暂停、回桌面恢复
    // （macOS/Windows 有前台应用观察者；Linux 后端是空实现，开关暂不生效果详见 linux.rs）
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    platform::store_app_handle(app);
    platform::start_auto_pause_observer(app);
    // 交互态下点桌面不一定触发前台切换（壁纸窗无边框不能成为 key），
    // 补一条「点击落在壁纸窗口 = 回到桌面」的直接恢复信号（仅 macOS 有实现）
    platform::start_desktop_click_monitor(app);
    // 指针注入：非交互态（壁纸在图标下方）收不到真实鼠标，靠轮询系统光标补上
    pointer::start(app, current_interactive(app));
    // macOS：清掉上次运行退出时留下的独占数据存储（退出走的是 Tauri 统一销毁，
    // 来不及回收进程/存储）。延迟一点做，别和启动抢 IO。
    #[cfg(target_os = "macos")]
    sweep_stale_data_stores(app, Duration::from_secs(3));
    tracing::info!("wallpaper engine ready");
    Ok(())
}

/// macOS：扫一遍并删掉「没人再用」的独占数据存储 —— 上次运行退出时留下的那些。
///
/// 快路上结束 WebContent 进程之后那份存储就没人管了（[`reap_data_store`] 只在
/// about:blank 老路上用，见那里的说明），所以**磁盘目录靠这里收**：`delay=3s`
/// 清上次运行留下的，监控每 60s 再跑一次清这一轮销毁窗口留下的。按
/// `fetchAllDataStoreIdentifiers` 扫：默认/非持久存储不在这个列表里（主窗口那套
/// UI 缓存不受影响），当前仍登记在 `state.data_stores` 里的一律跳过。
#[cfg(target_os = "macos")]
fn sweep_stale_data_stores(app: &AppHandle, delay: Duration) {
    let app = app.clone();
    if !custom_data_store_available() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let ids = match app.fetch_data_store_identifiers().await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::debug!("data store sweep: 枚举失败（{e}）");
                return;
            }
        };
        let in_use: Vec<[u8; 16]> = app
            .try_state::<WallpaperEngineState>()
            .map(|st| st.data_stores.lock().unwrap().values().copied().collect())
            .unwrap_or_default();
        let mut removed = 0usize;
        for id in ids {
            if in_use.contains(&id) {
                continue;
            }
            if app.remove_data_store(id).await.is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            tracing::info!(
                "data store sweep: 清掉 {removed} 份没人用的壁纸数据存储（上次遗留 / 已销毁窗口）"
            );
        }
    });
}

/// 一次性会话迁移：旧版本 macOS 把 CGDirectDisplayID（重启后会变）直接当
/// display_id 持久化，稳定 id 上线后按当前已连接显示器的 旧→新 映射改写会话键。
/// 键冲突（新 id 已有会话）时旧行是过期副本，直接删掉；未连接显示器的旧行保留
/// （无法反查 UUID），下次手动应用时覆盖。
fn migrate_display_ids(app: &AppHandle) {
    let pairs = platform::legacy_display_ids();
    if pairs.is_empty() {
        return;
    }
    let Some(db) = app.try_state::<Arc<Mutex<rusqlite::Connection>>>() else {
        return;
    };
    let Ok(conn) = db.lock() else {
        return;
    };
    for (old, new) in pairs {
        let moved = conn
            .execute(
                "UPDATE OR IGNORE wallpaper_sessions SET display_id = ?1 WHERE display_id = ?2",
                rusqlite::params![new, old],
            )
            .unwrap_or(0);
        if moved > 0 {
            tracing::info!("display id migrated: {old} -> {new}");
        } else {
            let _ = conn.execute(
                "DELETE FROM wallpaper_sessions WHERE display_id = ?1",
                [&old],
            );
        }
    }
}

/// 读某屏的持久化会话配置（显示器热插拔回归时精确恢复它自己的壁纸，
/// 而不是退化为「最近一次全局配置」；stop 删行后不会复活）。
fn load_db_session(app: &AppHandle, display_id: &str) -> Option<WallpaperConfig> {
    let db = app.try_state::<Arc<Mutex<rusqlite::Connection>>>()?;
    let conn = db.lock().ok()?;
    let raw: String = conn
        .query_row(
            "SELECT config_json FROM wallpaper_sessions WHERE display_id = ?1",
            [display_id],
            |r| r.get(0),
        )
        .ok()?;
    serde_json::from_str(&raw).ok()
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
            // 逐条留痕：启动后若「没恢复壁纸」，看这一行就知道是库里的会话记录没了/解析失败，
            // 还是记录在、但后续建窗环节被跳过（配合 create_window 的日志定位）。
            tracing::info!(
                "restore wallpaper on display {display_id}: type={} src={:?} item={:?}",
                cfg.r#type,
                cfg.src,
                item_id_of(&cfg)
            );
            windows.insert(format!("wallpaper-{display_id}"), cfg);
        }
        *st.default.lock().unwrap() = most_recent;
    }
    tracing::info!("wallpaper sessions restored: {count}");
}

// ---------- 壁纸窗口 label 双变体（无缝切换） ----------
//
// 换壁纸走「旧窗保持播放 + 新窗后台就绪后替换」（见 [`seamless_swap`]），同一块屏
// 同一时刻可能有两扇窗口，label 在两个变体间交替：
//   wallpaper-<display_id>    基 label —— `state.windows` 的规范键、会话表 display_id、
//                             指针/滚轮派发都用它
//   wallpaper-<display_id>-b  交替变体（display id 是纯数字，不会自带 -b 后缀）
// 真实窗口经 [`wallpaper_window`] / [`wallpaper_windows`] 解析到活着的那扇/两扇。

/// 基 label → 两个候选 label（[基, 交替]）
fn label_variants(base: &str) -> (String, String) {
    (base.to_string(), format!("{base}-b"))
}

/// 任意壁纸窗口 label → 基 label（非壁纸窗口返回 None）
fn base_label_of(label: &str) -> Option<String> {
    if !label.starts_with("wallpaper-") {
        return None;
    }
    Some(label.strip_suffix("-b").unwrap_or(label).to_string())
}

/// 基 label 下所有活着的壁纸窗口（无缝切换期间新旧两扇都在）。
/// 热更新（eval）与几何更新遍历它，别漏掉正在渐入的新窗。
fn wallpaper_windows(app: &AppHandle, base: &str) -> Vec<WebviewWindow> {
    let (a, b) = label_variants(base);
    [a, b]
        .iter()
        .filter_map(|l| app.get_webview_window(l))
        .collect()
}

/// 基 label 现役的壁纸窗口（存在性判断用；两扇都在时任取其一）。
/// pub(crate)：系统壁纸抽帧 / MCP 截图按 label 取窗也要过它（无缝切换后
/// 现役窗可能是 `-b` 变体）。
pub(crate) fn wallpaper_window(app: &AppHandle, base: &str) -> Option<WebviewWindow> {
    wallpaper_windows(app, base).into_iter().next()
}

/// 确保每个活动显示器都有壁纸窗口（创建/缩放/回收）
fn ensure_windows(app: &AppHandle) {
    ensure_windows_inner(app, platform::display_asleep());
}

/// 「没有会话配置所以不建窗」只提示一次，避免每 2s 刷屏
static NO_CONFIG_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 上一轮 ensure_windows 观察到的显示器集合（热插拔变化检测 → 事件推送前端）
static LAST_SEEN_SCREENS: Mutex<Option<std::collections::HashSet<u32>>> = Mutex::new(None);

/// `display_asleep` 由调用方传入：monitor 用「CG 报告 && 音频样本停止流动」的
/// 复合判定（CGDisplayIsAsleep 的进程内状态在合盖唤醒后可能卡死在 true，
/// 样本恢复流动即证明系统实际已唤醒）。
fn ensure_windows_inner(app: &AppHandle, display_asleep: bool) {
    // NSScreen 名称缓存刷新（本函数的正常调用方在主线程：2s 监控 tick 与 init；
    // 非主线程调用内部直接返回，名称读缓存兜底）
    platform::refresh_display_meta();
    // 「暂停释放内存」挂起期间不建窗：自动暂停已把壁纸渲染器整个销毁，
    // 这里若照常同步会把刚结束的进程立刻建回来（等于没释放）。恢复播放时
    // 由 resume_all 清标志后主动重建。
    if let Some(st) = app.try_state::<WallpaperEngineState>() {
        if *st.released.lock().unwrap() {
            return;
        }
    }
    // 显示器睡眠/唤醒切换期间不做任何窗口增删：此时 CGGetActiveDisplayList
    // 可能返回空列表（显示器从「活动」列表暂时消失），若照常执行下方清理逻辑，
    // 会把所有壁纸窗口误判为「已断开的显示器」全部销毁 —— 主窗口此时通常也是
    // 关闭/隐藏状态，最后一个窗口关闭就会触发 Tauri 默认行为退出整个进程
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
    // 显示器集合变化（热插拔/合盖开合）→ 通知前端刷新显示器管理页。
    // 首轮只记录不推送（启动时没有监听方，也没变化可言）。
    {
        let ids: std::collections::HashSet<u32> = screens.iter().map(|s| s.id).collect();
        let mut last = LAST_SEEN_SCREENS.lock().unwrap();
        if matches!(&*last, Some(prev) if prev != &ids) {
            tracing::info!("displays changed: {} -> {} screens", last.as_ref().map(|s| s.len()).unwrap_or(0), ids.len());
            let _ = app.emit("displays-changed", ());
        }
        *last = Some(ids);
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

    // 移除已断开的显示器窗口。按**真实活着的窗口**盘点（而不是 state.windows
    // 的键）：无缝切换的交替变体 / 进行中的新窗也在场，基 label 不在需求集合
    // 就整对收掉；屏还在则一律不动（切换半成品由换壁纸任务自己收尾）。
    for (l, w) in app.webview_windows() {
        let Some(base) = base_label_of(&l) else {
            continue;
        };
        if !desired.contains_key(&base) {
            destroy_wallpaper_window(app, &w);
            if let Ok(mut windows) = state.windows.lock() {
                windows.remove(&base);
            }
        }
    }

    // 创建/缩放窗口。
    // 没有任何壁纸配置（首次安装、从未应用过）时不建窗：桌面保持系统壁纸，
    // 不要弹出一个「降级提示页」占着桌面。用户应用第一张壁纸时再由
    // apply_on_main 按需建窗。
    for (label, (id, x, y, w, h)) in &desired {
        // 配置优先级：内存会话 > 本屏的持久化会话（拔掉的屏插回来时按 DB 行
        // 精确恢复它自己的壁纸，而不是退化为「最近一次全局配置」）> 最近一次配置
        let cfg_opt = configs
            .get(label)
            .cloned()
            .or_else(|| load_db_session(app, &id.to_string()))
            .or_else(|| default_cfg.clone());
        let Some(cfg) = cfg_opt else {
            // 启动后「一直不出壁纸」时，这一行是最直接的线索：库里没有任何会话配置
            if !NO_CONFIG_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::info!(
                    "ensure_windows: {label} 没有可用的会话配置（首次安装 / 会话已清空），不建壁纸窗口"
                );
            }
            continue;
        };
        let live = wallpaper_windows(app, label);
        if live.is_empty() {
            if let Err(e) = create_desktop_window(app, label, &cfg, (*x, *y, *w, *h)) {
                tracing::error!("create wallpaper window {label} failed: {e}");
                continue;
            }
        } else {
            for win in live {
                platform::set_frame(&win, *x, *y, *w, *h);
            }
        }
    }
}

/// 内容服务器的实际监听端口。真实端口在本进程内的 `Arc<Mutex<u16>>` 里（服务器绑定后异步写入），
/// `ContentServerState.port` 恒为占位 0；未就绪（启动早期）返回 None。
fn content_port(app: &AppHandle) -> Option<u16> {
    let port = *app.try_state::<Arc<Mutex<u16>>>()?.lock().ok()?;
    (port > 0).then_some(port)
}

fn media_base(app: &AppHandle) -> Option<String> {
    let port = content_port(app)?;
    let state = app.try_state::<ContentServerState>()?;
    Some(format!("http://127.0.0.1:{port}/media/{}", state.token))
}

/// web 壁纸站点根基址（绝对路径引用可解析）
fn web_base(app: &AppHandle) -> Option<String> {
    let port = content_port(app)?;
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
pub(crate) fn item_id_of(cfg: &WallpaperConfig) -> Option<String> {
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

/// 两次配置是否指向同一张壁纸。用于 decide「热更新还是销毁重建窗口」：
/// item 能从 src 解析出来时按 item 比（src 里的 token 会随运行期刷新，
/// 直接比 src 会把同一条目误判成切换）；解析不出时退回比 type + src。
fn same_wallpaper(a: &WallpaperConfig, b: &WallpaperConfig) -> bool {
    if a.r#type != b.r#type {
        return false;
    }
    match (item_id_of(a), item_id_of(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a.src == b.src,
    }
}

/// 装配一份「可以直接喂给渲染器」的配置：刷新媒体基址与 token、叠加
/// 「全局默认 + 本壁纸覆盖」的播放设置。
///
/// 建窗（[`create_desktop_window`]）与换壁纸（就地导航 / 重建窗口两条路）共用
/// 这一份，保证各条路径下发给渲染器的配置逐字段一致 —— 新增一个字段只改这里。
fn prepare_cfg(app: &AppHandle, cfg: &WallpaperConfig) -> WallpaperConfig {
    let mut cfg = cfg.clone();
    cfg.media_base = media_base(app);
    // 会话恢复来的 src 可能带上次运行的过期 token，用当前基址重写
    refresh_src(app, &mut cfg);
    // 全局默认 + 本壁纸覆盖（WE 的播放设置是按壁纸记忆的）
    let item = item_id_of(&cfg);
    apply_play_config(app, &mut cfg, item.as_deref());
    cfg
}

/// 渲染器页的完整 URL（换壁纸时就导航到这里）。
///
/// 端口就绪（进程内绝大多数时刻）直接按当前端口拼；未就绪（启动早期，内容
/// 服务器还没绑定）就沿用当前窗口的 scheme/host，只换路径与 query —— 那种
/// 情况下窗口本身也是用 `WebviewUrl::App(..)` 建的，同源换页即可。
fn renderer_url(
    app: &AppHandle,
    window: Option<&WebviewWindow>,
    cfg: &WallpaperConfig,
) -> Result<url::Url, String> {
    if let Some(port) = content_port(app) {
        return renderer_url_on(
            &format!("http://127.0.0.1:{port}"),
            cfg,
            content_token(app).as_deref(),
        );
    }
    let query = config_query_with_audio(cfg, content_token(app).as_deref());
    let mut url = window
        .ok_or("内容服务器端口未就绪")?
        .url()
        .map_err(|e| e.to_string())?;
    url.set_path("/renderer/index.html");
    url.set_query(Some(query.trim_start_matches('?')));
    Ok(url)
}

/// 渲染器页 URL（给定内容服务器 origin）。
///
/// 建窗与换壁纸整页导航共用这一份，只差一个 origin：**query 必须逐字段一致**。
/// 渲染器的全部壁纸配置都取自 URL query（`renderer/src/main.ts` 的 `initialCfg`），
/// 少一个字段就是「换了壁纸但设置没跟着换」。
fn renderer_url_on(
    origin: &str,
    cfg: &WallpaperConfig,
    audio_token: Option<&str>,
) -> Result<url::Url, String> {
    let query = config_query_with_audio(cfg, audio_token);
    format!("{origin}/renderer/index.html{query}")
        .parse::<url::Url>()
        .map_err(|e| format!("无效的渲染器 URL: {e}"))
}

/// 在既有壁纸窗口里**整页导航**到新配置（非 macOS 的换壁纸走这条路；macOS 的
/// 取舍见 [`new_data_store_id`]）。与热更新 `setWallpaper` 的区别是
/// 换的是文档：旧页面的 `pagehide` teardown 会跑完（销毁库实例、释放 pkg 缓存、
/// `loseContext`、撤销 blob），WebKit 随文档销毁一并回收 GPU 侧资源。
fn navigate_to_config(
    app: &AppHandle,
    window: &WebviewWindow,
    cfg: &WallpaperConfig,
) -> Result<(), String> {
    let cfg = prepare_cfg(app, cfg);
    let url = renderer_url(app, Some(window), &cfg)?;
    window.navigate(url).map_err(|e| e.to_string())
}

fn create_desktop_window(
    app: &AppHandle,
    label: &str,
    cfg: &WallpaperConfig,
    frame: (f64, f64, f64, f64),
) -> Result<WebviewWindow, String> {
    let cfg = prepare_cfg(app, cfg);
    // 渲染器页与媒体同源（内容服务器），消除跨源 fetch 限制
    let url = if let Some(port) = content_port(app) {
        WebviewUrl::External(renderer_url_on(
            &format!("http://127.0.0.1:{port}"),
            &cfg,
            content_token(app).as_deref(),
        )?)
    } else {
        let query = config_query_with_audio(&cfg, content_token(app).as_deref());
        WebviewUrl::App(format!("renderer/index.html{query}").into())
    };
    tracing::info!("create_window[{label}]: 开始建窗（type={}）", cfg.r#type);
    let build_started = std::time::Instant::now();
    // macOS：每块壁纸窗口独占一份 WKWebsiteDataStore，窗口销毁时才有机会连带
    // 回收它的 WebContent 进程，详见 [`new_data_store_id`]。其它平台该 builder
    // 项被忽略（wry 里仅 Apple 生效）。
    #[cfg(target_os = "macos")]
    let data_store = custom_data_store_available().then(new_data_store_id);
    let mut builder = WebviewWindowBuilder::new(app, label, url)
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
        .focused(false);
    #[cfg(target_os = "macos")]
    {
        if let Some(id) = data_store {
            builder = builder.data_store_identifier(id);
        }
    }
    let window = builder.build().map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    if let Some(id) = data_store {
        if let Some(st) = app.try_state::<WallpaperEngineState>() {
            st.data_stores.lock().unwrap().insert(label.to_string(), id);
            // 标识打出来是为了和磁盘上的 `~/Library/WebKit/<app>/WebsiteDataStore/<uuid>`
            // 对上：那条目录还在 = 这份存储没被回收成功（进程仍压着内存）。
            tracing::info!(
                "create_window[{label}]: 独占数据存储已登记（uuid={}，销毁窗口时回收）",
                store_id_label(&id)
            );
        }
    }
    tracing::info!(
        "create_window[{label}]: 窗口已建立（{}ms）",
        build_started.elapsed().as_millis()
    );

    // ⚠️ tauri-plugin-desktop-underlay 只用**窗口 label**记录「是否已下沉到桌面层」，
    // 且窗口销毁时它不会清理这条记录。同一个 label 的窗口销毁重建后，`is_desktop_underlay()`
    // 会返回陈旧的 true，`set_desktop_underlay(true)` 被判为「已是 underlay」而 no-op，
    // 新窗口从未真正下沉。建窗是句柄唯一更替的时机，先强制清掉陈旧记录。
    // （Windows 后端已自管父子化、不再用该插件，这里主要为 Linux 那条路径兜底。）
    if window.is_desktop_underlay() {
        if let Err(e) = window.set_desktop_underlay(false) {
            tracing::warn!("create_window[{label}]: 清理陈旧 underlay 状态失败: {e}");
        }
        tracing::info!("create_window[{label}]: 清掉同 label 遗留的 underlay 状态（重建窗口）");
    }

    platform::apply_desktop_window(&window, frame, current_interactive(app));
    tracing::info!("create_window[{label}]: 桌面层/几何已应用");

    window.show().map_err(|e| e.to_string())?;
    tracing::info!("create_window[{label}]: show() 完成");
    // 黑屏加固：窗口是 visible(false) 创建的，建窗瞬间 apply 时 occlusionState
    // 尚无 visible 位（实测 8192），WebKit 可能把页面判为不可见而停帧（黑屏）。
    // 按既有经验「show 之后重设层级 + orderFrontRegardless」，show 后再 apply
    // 一次（幂等），此时窗口已可见，遮挡态与合成层级都被矫正。
    platform::apply_desktop_window(&window, frame, current_interactive(app));
    tracing::info!("wallpaper window {label} created: {cfg:?}");
    crate::mem_watch::report("新建壁纸窗口");
    // ⚠️ 只在 Windows 上做「延迟重挂」：新建的窗口是「从未显示过」的状态，此时直接
    // 挂到 Win11 的 raised-desktop 层实测不生效（壁纸被原生壁纸盖住）；而手动开一次
    // 「隐藏图标」再关掉 —— 也就是**先脱离、再挂回** —— 就正确。这里直接复刻这个
    // 已被验证有效的动作：等窗口显示/合成稳定后，先按交互态脱离一次，再按真实设置
    // 挂回去（两步都是幂等的不重建窗口）。
    //
    // 写成「所有平台都编译、运行时才判平台」而不是 #[cfg(windows)]：这段只用跨平台
    // API，放进 cfg 里在 macOS 上根本不会被编译，借用/类型错误只能等 CI 的 Windows
    // 作业才暴露（这次就踩了一次 E0505）。
    {
        let app2 = app.clone();
        let label2 = label.to_string();
        tauri::async_runtime::spawn(async move {
            if !cfg!(target_os = "windows") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(800)).await;
            let app3 = app2.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = app2.run_on_main_thread(move || {
                if let Some(w) = app3.get_webview_window(&label2) {
                    let real = current_interactive(&app3);
                    // 复刻「开一次隐藏图标 → 再关掉」：先脱离，再按真实设置挂回
                    platform::apply_desktop_window(&w, frame, true);
                    platform::apply_desktop_window(&w, frame, real);
                    tracing::info!(
                        "create_window[{label2}]: 延迟重挂桌面层完成（real_interactive={real}）"
                    );
                }
                let _ = tx.send(());
            });
            let _ = rx.recv_timeout(Duration::from_secs(2));
        });
    }
    Ok(window)
}

/// 正在换壁纸的 label 集合（防抖 + 单飞，所有平台共用）。
static RELOADING: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

/// 换壁纸（防抖 + 单飞）：连切时等目标稳定后只加载最后一张。
///
/// 所有平台统一走**无缝切换**（[`seamless_swap`]）：旧窗保持播放、新窗后台加载，
/// 首帧 ready 后渐入、渐入走完再收旧窗 —— 慢加载的壁纸不再露出加载空白，加载
/// 失败时旧壁纸继续留在屏上。就地导航 / 先销毁后重建都会立刻抹掉旧画面，做不到
/// 无缝（macOS 就地导航在重内容页上还会冻死，见 [`destroy_wallpaper_window`]）。
fn schedule_window_reload(app: &AppHandle, label: &str, frame: (f64, f64, f64, f64)) {
    use std::time::Duration;
    /// 连切合并窗口：目标稳定这么久才动手
    const DEBOUNCE: Duration = Duration::from_millis(350);

    let set = RELOADING.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    {
        let mut g = set.lock().unwrap();
        if !g.insert(label.to_string()) {
            return; // 已有任务在跑，它会读到最新配置
        }
    }
    let app = app.clone();
    let label = label.to_string();
    tauri::async_runtime::spawn(async move {
        let done = || {
            if let Some(set) = RELOADING.get() {
                set.lock().unwrap().remove(&label);
            }
        };
        let current = |app: &AppHandle| {
            app.try_state::<WallpaperEngineState>()
                .and_then(|st| st.windows.lock().unwrap().get(&label).cloned())
        };

        // 防抖：目标还在变就继续等，稳定 DEBOUNCE 后才开工
        let mut last: Option<WallpaperConfig> = None;
        loop {
            let Some(cur) = current(&app) else {
                done();
                return; // 会话已清（stop/退出）
            };
            if last.as_ref().is_some_and(|p| same_wallpaper(p, &cur)) {
                break;
            }
            last = Some(cur);
            tokio::time::sleep(DEBOUNCE).await;
        }
        // 收敛循环：无缝切换期间目标又变（连切）→ 作废本次、换最新目标再切一轮
        loop {
            // 「暂停释放内存」挂起期间不加载：配置已登记进 state.windows，
            // 恢复播放时按最新配置整窗重建；轮播也不该在后台把渲染进程建回来
            if is_released(&app) {
                done();
                return;
            }
            let Some(target) = current(&app) else {
                done();
                return;
            };
            match seamless_swap(&app, &label, frame, &target).await {
                SwapOutcome::Swapped => {
                    crate::mem_watch::report("换壁纸（无缝切换）");
                    if current(&app).is_some_and(|c| same_wallpaper(&c, &target)) {
                        done();
                        return;
                    }
                    // 收尾瞬间又切了新目标：继续收敛
                }
                SwapOutcome::Superseded => {
                    // 半成品已由 seamless_swap 回收，直接换最新目标重来
                }
                SwapOutcome::KeptOld(reason) => {
                    tracing::warn!("wallpaper {label}: 未切换（{reason}），旧壁纸继续留在屏上");
                    let _ = app.emit(
                        "wallpaper-load-failed",
                        serde_json::json!({ "label": label, "reason": reason }),
                    );
                    done();
                    return;
                }
            }
        }
    });
}

/// 无缝切换的结果
enum SwapOutcome {
    /// 新窗已就绪并完成渐入，旧窗已收掉
    Swapped,
    /// 切换目标被更新的配置取代（本次的半成品新窗已回收）
    Superseded,
    /// 新壁纸没准备好（加载失败等），旧壁纸留在屏上
    KeptOld(String),
}

/// 无缝切换一块屏的壁纸：旧窗保持播放，新窗后台加载，首帧 ready 后渐入、
/// 渐入走完再收旧窗（视觉上是叠化，不是跳变）。
///
/// - 首帧信号复用截图那套：渲染器 mount 到首帧才报 `ready`（/diag 回流），
///   大 scene.pkg 冷启动 30s+ 也不误判（[`renderer_ready_since`]）；
/// - 加载失败（渲染器报 `failed:`）→ 销毁新窗、返回 [`SwapOutcome::KeptOld`]，
///   旧壁纸留在屏上 —— 「资源没准备好前，继续上一张」的语义就落在这条分支；
/// - 等待期间目标又被改掉（连切）→ 销毁新窗、返回 [`SwapOutcome::Superseded`]；
/// - 渐入由渲染器页面自己完成（wrap opacity 0→1，0.7s），宿主等它走完才收旧窗；
///   收旧窗前先把旧窗音量归零，避免叠化期间双声重叠。
async fn seamless_swap(
    app: &AppHandle,
    base: &str,
    frame: (f64, f64, f64, f64),
    target: &WallpaperConfig,
) -> SwapOutcome {
    use std::time::Duration;
    /// 渐入时长（渲染器 wrap 的 transition 0.7s + 裕量）
    const FADE: Duration = Duration::from_millis(900);
    /// 等首帧上限。渲染器自身的 90s 硬兜底会先报 failed（见 mountViaLib），
    /// 这里多留一拍只作最后防线 —— 播着旧壁纸远好过换上一张空白。
    const READY_TIMEOUT: Duration = Duration::from_secs(95);

    if is_released(app) {
        return SwapOutcome::KeptOld("「暂停释放内存」挂起中".into());
    }
    // 起点先于建窗：ready/failed 时间戳要「晚于它」才算本次的结果
    let t0 = crate::system_wallpaper::ready_stamp(app);
    let item = item_id_of(target);

    // 双变体交替占 label：旧窗在基 label 就让新窗去 `-b`，反之亦然；
    // 旧窗不在（首次应用）时新窗直接占基 label，没有旧窗可保。
    let old = wallpaper_window(app, base);
    let (va, vb) = label_variants(base);
    let incoming = match &old {
        Some(w) => {
            if w.label() == va {
                vb
            } else {
                va
            }
        }
        None => va,
    };

    // 主线程建新窗（透明窗口；渲染器页面 opacity 0 起步，就绪后渐入）
    let created = {
        let (tx, rx) = std::sync::mpsc::channel();
        let app2 = app.clone();
        let incoming2 = incoming.clone();
        let target2 = target.clone();
        let _ = app.run_on_main_thread(move || {
            let r = create_desktop_window(&app2, &incoming2, &target2, frame).map(|_| ());
            let _ = tx.send(r);
        });
        rx.recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| Err("建窗超时".into()))
    };
    if let Err(e) = created {
        return SwapOutcome::KeptOld(format!("新窗口创建失败：{e}"));
    }

    // 等首帧 / 失败 / 目标变更 / 超时
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        // 目标被取代（含 stop 清会话）：回收半成品，交给调用方换最新目标
        let superseded = app
            .try_state::<WallpaperEngineState>()
            .and_then(|st| st.windows.lock().unwrap().get(base).cloned())
            .map(|c| !same_wallpaper(&c, target))
            .unwrap_or(true);
        if superseded {
            destroy_incoming(app, &incoming);
            return SwapOutcome::Superseded;
        }
        if let Some(reason) = crate::content_server::failure_since(app, t0, item.as_deref()) {
            destroy_incoming(app, &incoming);
            return SwapOutcome::KeptOld(reason);
        }
        if renderer_ready_since(app, t0, item.as_deref()) {
            break;
        }
        if start.elapsed() > READY_TIMEOUT {
            tracing::warn!(
                "wallpaper {base}: 等首帧超时（{}s），强制替换",
                READY_TIMEOUT.as_secs()
            );
            break;
        }
    }

    // 渐入收尾：旧窗先收音量，等渐入走完再销毁，叠化过渡
    if let Some(w) = &old {
        let _ = w.eval("window.__wp && window.__wp.setVolume(0)");
    }
    tokio::time::sleep(FADE).await;
    if let Some(w) = old {
        if app.get_webview_window(w.label()).is_some() {
            destroy_wallpaper_window(app, &w);
        }
    }
    SwapOutcome::Swapped
}

/// 半成品新窗回收（中止 / 被取代时）
fn destroy_incoming(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        destroy_wallpaper_window(app, &w);
    }
}

/// 新壁纸首帧是否就绪：ready 时间戳不早于 t0，且归属目标条目（None = 不校验归属）。
fn renderer_ready_since(app: &AppHandle, t0: u64, item: Option<&str>) -> bool {
    if crate::system_wallpaper::ready_stamp(app) < t0 {
        return false;
    }
    match item {
        Some(want) => crate::content_server::ready_item(app).as_deref() == Some(want),
        None => true,
    }
}

/// 销毁前那次主线程调用的结果（决定销毁后走哪条路）。
#[cfg(target_os = "macos")]
enum KillPlan {
    /// 还没动手就发现这块窗口已被新壁纸接管 → 取消销毁
    Superseded,
    /// 拿到 WebContent 进程的 pid：销毁后按 pid 结束它
    Pid(i32),
    /// 没有独占进程（老系统共享存储）或取不到 pid → 走 about:blank 老路
    Fallback,
}

/// 销毁壁纸窗口（壁纸窗口的所有销毁点统一走这里）。
///
/// macOS 上首选**先取 pid、再销毁、再按 pid 结束进程**：主线程上从 WKWebView 问出
/// WebContent 进程的 pid，`destroy()` 把窗口摘掉，然后向那个 pid 发 SIGKILL
///（[`macos::kill_web_content_process`]）。内存这时是实打实还回去的 —— 不用赌 WebKit
/// 的进程池会不会还、也不用再撞 `Data store is in use`（进程都死了，没谁攥着那份存储）。
///
/// 顺序不能反：pid 只能在窗口活着的时候问（销毁后 WKWebView 就没了）；而杀在销毁之后，
/// WebKit 就没有机会再为这扇已经摘掉的窗口重启一个进程。
///
/// pid 取不到时退回原来的三步（缺一不可）：
///
/// 1. 先导航到 `about:blank` —— 页面跑完 `pagehide` 的 teardown（销毁库实例、
///    释放 pkg 缓存、`loseContext`、撤销 blob），并关掉 SSE 等长连接。
/// 2. 隔一拍 `destroy()` —— 把 WKWebView 从窗口上摘下来。
/// 3. [`reap_data_store`] 删掉这块窗口独占的 WKWebsiteDataStore —— 删成功时这台
///    WebContent 进程才会退出、压着的几百 MB～1GB 才还回去。前两步之后页面是空了，
///    但进程仍被 WebKit 的进程池留着（实测切到视频 / 网页这类轻量壁纸后 footprint
///    依旧不降）。**第 3 步不保证成功**（UI 进程里只要还活着一个引用该存储的
///    `WKWebsiteDataStore`，WebKit 就报 `Data store is in use`），所以它只是尽力
///    而为：失败交给 [`sweep_stale_data_stores`] 与下次启动的清理。
///
/// 日常换壁纸：非 macOS 走 [`schedule_window_reload`] 的同窗口导航、不经本函数；
/// macOS 换壁纸销毁旧窗口时也走这里（取 pid + 结束进程）。另外 stop、显示器
/// 移除、非 macOS 导航失败降级重建也会调用。
///
/// 其它平台不需要这一圈：WebView2 / WebKitGTK 的 destroy 会连带销毁渲染进程。
#[cfg(target_os = "macos")]
fn destroy_wallpaper_window(app: &AppHandle, window: &WebviewWindow) {
    let w = window.clone();
    let app = app.clone();
    let label = w.label().to_string();
    // 标识必须**在这一刻**取出并摘掉：紧接着的同名重建会往同一个 label 写入新
    // 标识，异步回收再按 label 查就会拿到新窗口那份，把刚建好的壁纸的存储删掉。
    let mut store = app
        .try_state::<WallpaperEngineState>()
        .and_then(|st| st.data_stores.lock().unwrap().remove(&label));
    // 换页前的 URL：用来识别「等待期间有人又应用了新壁纸」
    let original = w.url().ok();
    // 有没有自己的 WebContent 进程 = 有没有独占数据存储（macOS 14+ 才有这 API）。
    // 共享存储的窗口是**几个窗口共用一个进程**，杀进程会连累别人，所以那种情况不杀。
    let owns_process = store.is_some();
    tauri::async_runtime::spawn(async move {
        // 首选：在**主线程**上问出这个窗口的 WebContent 进程 pid，销毁后按 pid 结束它
        //（[`macos::kill_web_content_process`]：为什么不能靠 WebKit 的两个私有选择器、
        //  也不能只靠 destroy()，那里写了实测）。进程一死，后面的按标识删存储也不会再
        // 撞 `Data store is in use`。pid 取不到时才退回下面那条老路。
        //
        // 动手之前必须和下面那条路一样先看 URL：停止壁纸后**立刻**重新应用（真实存在
        // 的操作顺序）时，新壁纸的整页导航可能已经先落在这块窗口上，此刻动手就是杀掉
        // 刚起来的那一页。检查与取 pid 放在同一次主线程调用里，两者之间不会有别的
        // 导航插进来（取到的一定是这一页的进程）。
        //
        // 只在窗口有独占数据存储时才杀（`owns_process`）：那种窗口按 WebKit 的规则
        // 必然独占一个 WebContent 进程（这正是前几轮拿独占存储换来的），杀它不会波及
        // 别的窗口；老系统上退化成共享存储（几个窗口一个进程）时不杀，走下面那条路。
        let (tx, rx) = tokio::sync::oneshot::channel();
        let w_kill = w.clone();
        let original_kill = original.clone();
        let _ = app.run_on_main_thread(move || {
            let superseded = match (&original_kill, w_kill.url()) {
                (Some(before), Ok(now)) => *before != now && now.scheme() != "about",
                _ => false,
            };
            // 「被接管」和「没有可杀的进程」必须是两个不同的值，不能都并成一句
            //「不杀」—— 前者要取消整个销毁，后者只是退回 about:blank 老路。
            let plan = if superseded {
                KillPlan::Superseded
            } else if owns_process {
                match macos::web_content_pid(&w_kill) {
                    Some(pid) => KillPlan::Pid(pid),
                    None => KillPlan::Fallback,
                }
            } else {
                KillPlan::Fallback
            };
            let _ = tx.send(plan);
        });
        match rx.await.unwrap_or(KillPlan::Fallback) {
            KillPlan::Superseded => {
                restore_data_store(&app, &label, store.take());
                tracing::debug!("wallpaper window {label}: 销毁前已换上新的壁纸，取消本次销毁");
                return;
            }
            KillPlan::Pid(pid) => {
                let _ = w.destroy();
                // 等窗口真的从注册表里消失再动手。`destroy()` 是**异步投递**的，刚调用完
                // 的那一瞬间 WKWebView 还在，此时杀掉它的进程等于告诉 WebKit「这个还活着
                // 的页面崩了」，它会立刻补一个新进程 —— 补出来的是空页、只有 ~9MB，但
                // **每销毁一扇窗口就多一个**：实测 12 轮「应用→停止」之后池子里躺了 19 个
                //（171MB），成了另一条慢漏。页面彻底没了再杀，池子不补人（实测单独杀掉
                // 池中的空闲进程，WebKit 不会补）。
                for _ in 0..40 {
                    if app.get_webview_window(&label).is_none() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                // 杀进程要等它真消失（上限 500ms），别占着异步运行时的工作线程
                let killed =
                    tokio::task::spawn_blocking(move || macos::kill_web_content_process(pid))
                        .await
                        .unwrap_or(false);
                // 这里**刻意不**去删这份独占数据存储：它当初的存在理由是「删掉它才能让
                // WebContent 退出」，而进程现在已经显式结束了，剩下的只是一份磁盘目录
                // —— 偏偏刚死的这一刻删一定撞 `DataStoreInUse`（销毁后的 WKWebView 还
                // 没 dealloc），实测每次都要白等 20~30s 再记一条吓人的「回收失败」。
                // 目录交给 [`sweep_stale_data_stores`]（60s 一轮）与下次启动清理。
                crate::mem_watch::report(if killed {
                    "销毁壁纸窗口（已结束 WebContent 进程）"
                } else {
                    "销毁壁纸窗口（结束 WebContent 进程失败）"
                });
                return;
            }
            KillPlan::Fallback => {}
        }
        let _ = w.eval("window.location.replace('about:blank')");
        // 等 about:blank **真的换上**再销毁。原先固定 200ms 是抢跑：4K 场景页那几 MB
        // 的文档换页 + teardown 常要几百 ms，导航还没提交就把 WKWebView 摘下来，
        // WebKit 留在进程池里的那份进程便仍压着**整张壁纸**（实测活动监视器里
        // 出现过以 `about:` 记名、常驻 1.39GB 的残留进程）；等提交后再销毁，池里
        // 留下的是空页（实测同批残留里那几个只占 37~66MB）。页面僵死时导航可能永远
        // 不提交，所以等待有上限 —— 到点照旧销毁，退回原来的行为。
        let started = std::time::Instant::now();
        let deadline = started + Duration::from_millis(2500);
        let mut committed = false;
        let mut reapplied = false;
        while std::time::Instant::now() < deadline {
            match w.url() {
                Ok(u) if u.scheme() == "about" => {
                    committed = true;
                    break;
                }
                // 既不是原页、也不是 about: = 等待期间有人把这块窗口导航到新壁纸了
                //（停止壁纸后立刻重新应用）。那就别销毁：留着它服务新壁纸。
                Ok(u) if original.as_ref().is_some_and(|o| *o != u) => {
                    reapplied = true;
                    break;
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if reapplied {
            restore_data_store(&app, &label, store.take());
            tracing::debug!("wallpaper window {label}: 销毁前已换上新的壁纸，取消本次销毁");
            return;
        }
        if committed {
            // 提交即已跑完 pagehide 的 teardown，再给一拍让它收尾
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // 记下这次等待的结果：「抢跑」到底还发不发生、常发还是偶发，看这条日志说话
        //（2.5s 到点 = 页面僵死，退回「不等就销毁」的旧行为，会留下压着整张壁纸的残留）
        if committed {
            tracing::info!(
                "wallpaper window {label}: about:blank 已提交（等待 {}ms）→ 销毁并回收存储",
                started.elapsed().as_millis()
            );
        } else {
            tracing::warn!(
                "wallpaper window {label}: 等 2.5s about:blank 仍未提交，按原行为销毁（本次残留进程可能仍压着旧壁纸）"
            );
        }
        let _ = w.destroy();
        if let Some(uuid) = store {
            reap_data_store(&app, &label, uuid).await;
        }
        crate::mem_watch::report("销毁壁纸窗口（about:blank 路径）");
    });
}

/// 销毁被取消时，把窗口的数据存储标识放回登记表 —— 窗口还在服务新壁纸，标识得跟着它，
/// 否则后续销毁点会找不到这份存储，留下永远删不掉的残留目录。
#[cfg(target_os = "macos")]
fn restore_data_store(app: &AppHandle, label: &str, store: Option<[u8; 16]>) {
    let (Some(uuid), Some(st)) = (store, app.try_state::<WallpaperEngineState>()) else {
        return;
    };
    st.data_stores
        .lock()
        .unwrap()
        .entry(label.to_string())
        .or_insert(uuid);
}

/// 非 macOS：直接销毁（见 [`destroy_wallpaper_window`] 的说明）
#[cfg(not(target_os = "macos"))]
fn destroy_wallpaper_window(_app: &AppHandle, window: &WebviewWindow) {
    let _ = window.destroy();
}

/// macOS：按标识的 WKWebsiteDataStore 是 macOS 14（Darwin 23）才有的 API。
///
/// 更低版本 wry 会退回**共享**存储 —— 那就没有可回收的存储，这里直接判否：
/// 既不挂标识（挂上也无效），也不去试删除（否则每次销毁窗口都白跑一轮删除重试并
/// 留一条 warn，让人误以为回收失败）。
#[cfg(target_os = "macos")]
fn custom_data_store_available() -> bool {
    let mut buf = std::mem::MaybeUninit::<libc::utsname>::uninit();
    if unsafe { libc::uname(buf.as_mut_ptr()) } != 0 {
        return false;
    }
    let buf = unsafe { buf.assume_init() };
    let release = unsafe { std::ffi::CStr::from_ptr(buf.release.as_ptr()) };
    let major: u32 = release
        .to_string_lossy()
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    major >= 23
}

/// macOS：给一块壁纸窗口分配**独占**的 WKWebsiteDataStore 标识。
///
/// 背景（「换壁纸后旧壁纸的内存不释放」）：WebKit 把 WebContent 进程挂在数据
/// 存储上 —— 默认（共享）存储下，`destroy()` 只是把 WKWebView 从窗口上摘下来，
/// 进程连同它压着的整页（JS 堆 / WebGL 上下文 / 解析好的 `scene.pkg` / 视频
/// 解码器）留在进程池里，之后新建的窗口又落回同一个池子，于是内存回不来：实测
/// 活动监视器里那个 `http://127.0.0.1:<port>` 进程，切到视频/网页这种轻量壁纸
/// 后仍是场景留下的几百 MB～1GB。给每块窗口一份独立存储，就是让每块窗口**独占一个**
/// WebContent 进程 —— 于是销毁窗口时可以直接结束它（[`destroy_wallpaper_window`]）：
/// 独占意味着不会误伤别的窗口，也不必等 WebKit 哪天心情好才肯回收。删掉存储本身
/// 只是顺带清磁盘（[`reap_data_store`] 那条老路 + [`sweep_stale_data_stores`]）。
///
/// 独占存储服务于真正销毁窗口的场合：macOS 换壁纸（旧窗口销毁后重建）、stop、
/// 显示器移除、非 macOS 导航失败降级重建；非 macOS 的日常换壁纸走同窗口导航。
///
/// 代价：壁纸页的 localStorage / IndexedDB 每块窗口（每次重建）都是全新的。WE 的
/// 用户属性走 project.json + `/props` 注入，不依赖它；网页壁纸自己写
/// localStorage 的自定义状态会随重建丢失 —— 与「每切一张常驻几百 MB」相比这个
/// 取舍是划算的。
#[cfg(target_os = "macos")]
fn new_data_store_id() -> [u8; 16] {
    use rand::RngCore;
    let mut id = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut id);
    id
}

/// macOS：把数据存储标识打成 WebKit 在磁盘上用的那种 UUID 文本
/// （`8-4-4-4-12`）——`NSUUID::from_bytes` 是逐字节映射，所以这里的十六进制顺序
/// 和 `~/Library/WebKit/<app>/WebsiteDataStore/<uuid>` 的目录名一致，日志里的
/// 标识可以直接拿去磁盘上查找残留。
#[cfg(target_os = "macos")]
fn store_id_label(id: &[u8; 16]) -> String {
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// macOS：删除某块壁纸窗口的独占数据存储（**只在 [`destroy_wallpaper_window`] 的
/// about:blank 老路上调用**；能拿到 pid 的那条快路直接结束进程，不再走这里）。
///
/// 老路上这一步是**还内存的关键**：删成功 = WebKit 销毁这个存储的进程池 = 那个压着
/// 整张壁纸的 WebContent 退出。而 `removeDataStoreForIdentifier` 在还有 WKWebView
/// 引用这份存储时会失败（wry 映射为 `DataStoreInUse`）：`destroy()` 是异步投递的，
/// Tauri 注册表里的条目消失也不等于 WKWebView 已经 dealloc（autorelease 池还要过
/// 一拍），所以这里带退避重试。
///
/// 注意这条路**不保证成功**（实测 macOS 26 上约七成失败，见
/// [`schedule_window_reload`]）：失败说明 UI 进程里还活着一个引用这份存储的
/// `WKWebsiteDataStore`（销毁后的 WKWebView 及其 configuration 还没 dealloc），
/// 那就只能等它自己松开 —— 所以重试窗口给到 20s，失败后交给 60s 一轮的
/// [`sweep_stale_data_stores`] 兜底（以及下次启动的清理）。
#[cfg(target_os = "macos")]
async fn reap_data_store(app: &AppHandle, label: &str, uuid: [u8; 16]) {
    /// 重试次数 × 间隔 = 20s 的回收窗口
    const ATTEMPTS: u32 = 40;
    let started = std::time::Instant::now();
    let id = store_id_label(&uuid);
    for attempt in 0..ATTEMPTS {
        match app.remove_data_store(uuid).await {
            Ok(()) => {
                let secs = started.elapsed().as_secs_f32();
                tracing::info!(
                    "wallpaper window {label}: 独占数据存储已删除（uuid={id}，耗时 {secs:.1}s，WebContent 进程随之退出）"
                );
                return;
            }
            Err(e) => {
                if attempt + 1 == ATTEMPTS {
                    // 失败最可能的原因是 WKWebView 还没真的 dealloc（`DataStoreInUse`）。
                    // 这条 warn 里带上标识，方便按
                    // `~/Library/WebKit/<app>/WebsiteDataStore/<uuid>` 查残留目录。
                    tracing::warn!(
                        "wallpaper window {label}: 独占数据存储删除失败（uuid={id}，{e}）；目录留给 sweep 与下次启动清理"
                    );
                } else {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }
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
        let cfg2 = prepare_cfg(app, &cfg);

        // 换了壁纸**不能**走 `setWallpaper` 热更新：同一文档里换实例实测会把
        // 上一张壁纸的 JS 堆与 GPU 资源一直攥着 —— 同一窗口逐张切换，Activity
        // Monitor 的 footprint 从 ~300MB 单调涨到 4GB+（复现 6 张场景
        // 305M→3085M），切到轻量视频也不回落。热更新换的是实例不是文档，回收
        // 全靠库自觉。
        //
        // 换壁纸（[`schedule_window_reload`]）按平台分两条路（见该函数注释）：
        //   - 非 macOS：同窗口整页导航 —— WebView2 / WebKitGTK 会连带销毁旧文档
        //   - macOS：销毁旧窗口（结束 WebContent 进程）→ 同名重建 —— 就地导航在
        //     重内容页上回收不可靠、会冻死
        //
        // 同一条目改 fit / dpr / fps / 属性时 item 不变 → 仍走热更新，不换页。
        let switched = state
            .windows
            .lock()
            .unwrap()
            .get(label)
            .map(|old| !same_wallpaper(old, &cfg2))
            .unwrap_or(false);

        if switched {
            // 只登记新配置，真正的换页交给防抖的单飞任务：连切时等目标稳定后
            // 只加载最后一张 —— 避免逐张都重建 GL 上下文、重传纹理把 GPU 顶成
            // 连续尖峰。
            state
                .windows
                .lock()
                .unwrap()
                .insert(label.clone(), cfg2.clone());
            // 防抖 + 单飞：连切时任务读最新配置、收敛到最后一张
            schedule_window_reload(app, label, *frame);
        } else {
            let window = match wallpaper_window(app, label) {
                Some(w) => w,
                None => {
                    create_desktop_window(app, label, &cfg, *frame).map_err(|e| e.to_string())?
                }
            };
            platform::apply_desktop_window(&window, *frame, current_interactive(app));
            // 页面还是空白（stop → 立刻重新应用：销毁流程把页面置了空，窗口
            // 200ms 后才真正销毁）：导航过去，别对着空页 eval setWallpaper。
            let blank = window.url().map(|u| u.scheme() == "about").unwrap_or(false);
            if blank {
                navigate_to_config(app, &window, &cfg2)?;
            } else {
                // 复用旧窗口：下发 setWallpaper 热更新（新建窗口的 URL 已带配置，
                // 但这里 UI 变更也会走到，eval 一次无副作用）。
                let js = format!(
                    "window.__wp && window.__wp.setWallpaper({})",
                    serde_json::to_string(&cfg2).map_err(|e| e.to_string())?
                );
                window.eval(&js).map_err(|e| e.to_string())?;
            }
            state
                .windows
                .lock()
                .unwrap()
                .insert(label.clone(), cfg2.clone());
        }

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

    // 会话已变化 → 前端刷新「已应用」徽章与显示器管理页
    let _ = app.emit("sessions-changed", ());

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
        for w in wallpaper_windows(app, &label) {
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
        let mut audio_perm_last: Option<bool> = None;
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
                    &mut audio_perm_last,
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
                // macOS：每分钟扫一遍没人用的数据存储 —— 快路上销毁窗口之后留下的
                // 那份（进程已结束，只是目录还占着盘），以及崩溃/强杀留下的孤儿存储，
                // 都等这里删。删除要等销毁后的 WKWebView 松开引用，所以不放在销毁
                // 那一刻做（实测那一瞬间必失败，见 [`reap_data_store`]）。
                #[cfg(target_os = "macos")]
                sweep_stale_data_stores(&app2, Duration::ZERO);
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
    #[allow(unused_variables)] audio_perm_last: &mut Option<bool>,
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
        // 判定阈值分两档，且**报出实测时延**：
        // - 100ms~1s：WARN。主线程在 WebKit 重内容合成 / 系统壁纸截图 / 4K 视频
        //   首帧解码期间偶发 100~300ms 停顿是正常的 —— 2026-09-11 日志里 30 次
        //   「100ms 未接手」全部落在重壁纸挂载后的那 1~3s 内，界面并无卡顿，
        //   一律报 ERROR 只会把排查引到错误方向（当天的排查就被它带偏过一次）。
        // - >1s：ERROR，这才是「软件无响应」级别的信号，告警留痕。
        let posted = std::time::Instant::now();
        for _ in 0..200 {
            if done.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let lag = posted.elapsed();
        if !done.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::error!(
                "main thread did not process monitor dispatch within {}ms (UI event loop wedged?)",
                lag.as_millis()
            );
        } else if lag > Duration::from_millis(100) {
            tracing::warn!(
                "main thread took {}ms to run the monitor dispatch（WebKit 重内容合成/截图期间常见）",
                lag.as_millis()
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
            // 开盖后自行恢复；从未成功过（无权限等永久性问题）不重试 —— 但
            // 例外是「用户刚在授权框点了允许」：权限从无到有的瞬间自动重试一次
            // （首次创建 Tap 时弹的授权框，允许后不必重启应用）。
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
            } else {
                #[cfg(target_os = "macos")]
                {
                    let granted = crate::audio_capture::macos_permission_granted();
                    if granted && *audio_perm_last == Some(false) {
                        tracing::info!("audio permission granted after failure; retrying capture");
                        action.restart_audio = true;
                    }
                    *audio_perm_last = Some(granted);
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
            for w in wallpaper_windows(&app, &label) {
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
    parts.push(format!("filter={}", url_encode(&cfg.filter)));
    // 渲染质量档位（库 1.3.23+；query 键 aa/pq/pp 与上游 bench 约定一致）
    parts.push(format!("aa={}", url_encode(&cfg.aa)));
    parts.push(format!("pq={}", url_encode(&cfg.particles)));
    parts.push(format!("pp={}", url_encode(&cfg.post_processing)));
    parts.push(format!("muted={}", cfg.muted));
    // 精确音量（0..1）：有壁纸专属音量记录时下发；渲染器加载完成后按它起音量
    if let Some(v) = cfg.volume {
        parts.push(format!("volume={v}"));
    }
    parts.push(format!("loop={}", cfg.r#loop));
    // WE 官方素材通路（库 1.4.1+）：1 时库挂载场景前探测 /api/local-assets，
    // 本机有官方 assets 树就用原版像素；生产构建不带该参数时库不会发探测
    if cfg.local_assets {
        parts.push("localAssets=1".to_string());
    }
    // 场景壁纸 project.json 声明的 pkg 入口（非默认命名/布局时给渲染器指路）
    if let Some(pkg) = &cfg.scene_pkg {
        parts.push(format!("scenePkg={}", url_encode(pkg)));
    }
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

// `async` 让命令体在异步线程池执行、而非主线程的 WebView2 IPC 回调内 ——
// Windows 上从 WebView2 回调里建 WebView2 会重入死锁（详见 apply_item_inner 注释）。
#[tauri::command(async, rename = "wallpaper_apply")]
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

#[tauri::command(async, rename = "wallpaper_stop")]
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
            // 两扇都收（无缝切换的基/交替变体可能同时在场）
            for w in wallpaper_windows(&app2, label) {
                destroy_wallpaper_window(&app2, &w);
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
        // 全停：连「最近一次配置」也清掉。否则监控的 ensure_windows 会拿
        // default 把窗口重新建回来，stop 等于没停 —— 清空壁纸后桌面应保持
        // 系统壁纸（与「首次安装不设壁纸」一致）。
        if display_id.is_none() {
            *state.default.lock().unwrap() = None;
        }
        // 「暂停释放内存」挂起标志一并清零：壁纸已停，恢复路径不会再走，
        // 留着 true 会让监控的 ensure_windows 永久不建窗（显示器布局变化
        // 无法同步）。部分停止只清对应 label 的话，全停之外的场景由
        // resume_all 的重建兜底，这里不必精细区分。
        *state.released.lock().unwrap() = false;
        let _ = app2.emit("sessions-changed", ());
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

/// 显示器列表 + 每屏当前会话摘要（显示器管理页 / MCP 用）。
/// `id` 与 `wallpaper_sessions.display_id`、`wallpaper_apply_item`/`wallpaper_stop`
/// 的 displayId 是同一套稳定 id。
#[tauri::command(async, rename = "wallpaper_displays_list")]
pub fn displays_list(app: AppHandle) -> Result<serde_json::Value, String> {
    // 非主线程调用时是空操作，名称走 2s 监控 tick 维护的缓存
    platform::refresh_display_meta();
    let screens = platform::active_screens();
    let state = app.try_state::<WallpaperEngineState>();
    let db = app.try_state::<Arc<Mutex<rusqlite::Connection>>>();

    // 每屏当前条目：内存会话优先、DB 兜底（与 ensure_windows 的恢复优先级一致）
    let mut item_ids: Vec<String> = Vec::new();
    let mut per_display: Vec<(String, Option<String>)> = Vec::new();
    for s in &screens {
        let id = s.id.to_string();
        let item_id = state
            .as_ref()
            .and_then(|st| {
                let windows = st.windows.lock().unwrap();
                windows.get(&format!("wallpaper-{id}")).and_then(item_id_of)
            })
            .or_else(|| {
                let conn = db.as_ref()?.lock().ok()?;
                conn.query_row(
                    "SELECT item_id FROM wallpaper_sessions WHERE display_id = ?1",
                    [&id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()?
            });
        if let Some(i) = &item_id {
            item_ids.push(i.clone());
        }
        per_display.push((id, item_id));
    }
    let summaries = crate::library::item_cover_summary(&app, &item_ids);
    // 模式 + 每屏轮播绑定（独立模式的每屏上下文摘要）
    let (mode, bindings) = match &db {
        Some(db) => match db.lock() {
            Ok(conn) => {
                let mode =
                    db::get_setting(&conn, "display_mode").unwrap_or_else(|| "unified".to_string());
                let mut bindings = HashMap::new();
                for s in &screens {
                    let id = s.id.to_string();
                    if let Some(ctx) = read_ctx(&conn, &id) {
                        let interval = ctx.playlist.interval_sec.max(30);
                        let last = clock_get(&id);
                        let next_at_ms = if rotation_paused(&conn) || last <= 0 {
                            None
                        } else {
                            Some((last + interval) * 1000)
                        };
                        bindings.insert(
                            id,
                            serde_json::json!({
                                "playlistId": ctx.playlist.id,
                                "playlistName": ctx.playlist.name,
                                "index": ctx.index,
                                "total": ctx.playlist.item_ids.len(),
                                "intervalSec": interval,
                                "nextAtMs": next_at_ms,
                            }),
                        );
                    }
                }
                (mode, bindings)
            }
            Err(_) => ("unified".to_string(), HashMap::new()),
        },
        None => ("unified".to_string(), HashMap::new()),
    };

    let displays: Vec<serde_json::Value> = screens
        .iter()
        .map(|s| {
            let id = s.id.to_string();
            let item_id = per_display
                .iter()
                .find(|(d, _)| d == &id)
                .and_then(|(_, i)| i.clone());
            let summary = item_id
                .as_ref()
                .map(|i| summaries.get(i).cloned().unwrap_or_else(|| (i.clone(), None)));
            let binding = bindings
                .get(&id)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            serde_json::json!({
                "id": id,
                "name": s.name,
                "x": s.x,
                "y": s.y,
                "w": s.w,
                "h": s.h,
                "scale": s.scale,
                "isPrimary": s.is_primary,
                "itemId": item_id,
                "title": summary.as_ref().map(|(t, _)| t.clone()),
                "previewUrl": summary.and_then(|(_, p)| p),
                "binding": binding,
            })
        })
        .collect();
    Ok(serde_json::json!({ "mode": mode, "displays": displays }))
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

/// 「暂停释放内存」是否处于挂起态（壁纸窗口已销毁、等恢复时重建）
fn is_released(app: &AppHandle) -> bool {
    app.try_state::<WallpaperEngineState>()
        .map(|st| *st.released.lock().unwrap())
        .unwrap_or(false)
}

/// 「自动暂停」触发时的统一入口（macOS/Windows 的前台切换观察者都走这里）：
/// 与手动 [`pause_all`] 的区别是会按设置叠加「暂停释放内存」——开关开启时
/// 直接销毁全部壁纸窗口（渲染进程随之结束，内存实打实归还），回桌面时由
/// [`resume_all`] 按保留的会话配置整窗重建。调用方负责置 `auto_paused` 标志。
pub fn auto_pause_enter(app: &AppHandle) -> Result<(), String> {
    pause_all(app.clone())?;
    if pause_release_enabled(app) {
        release_wallpaper_windows(app);
    }
    Ok(())
}

/// 「暂停释放内存」开关：`wallpaper_auto_pause_release`，默认关。
/// 与自动暂停一样由前台切换时直读 DB（低频事件，免缓存同步）。
fn pause_release_enabled(app: &AppHandle) -> bool {
    app.try_state::<Arc<Mutex<Connection>>>()
        .and_then(|db| {
            db.lock()
                .ok()
                .and_then(|c| db::get_setting(&c, "wallpaper_auto_pause_release"))
        })
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

/// 销毁全部壁纸窗口（「暂停释放内存」的释放动作）。
///
/// 会话配置（`state.windows` / `wallpaper_sessions`）**保留**：恢复播放时按它
/// 整窗重建。macOS 上 [`destroy_wallpaper_window`] 会连带结束每块窗口独占的
/// WebContent 进程 —— 这正是本开关的目的（页面内 release 只能还一部分，
/// 进程级 JS 堆/解码器/GPU 缓存要进程退出才彻底）。
fn release_wallpaper_windows(app: &AppHandle) {
    let Some(st) = app.try_state::<WallpaperEngineState>() else {
        return;
    };
    // 先置标志再动手：destroy 是异步投递的，2s 一轮的监控若插在「销毁完成」
    // 与「标志置位」之间，会按仍登记的配置把窗口原样建回来（等于没释放）
    *st.released.lock().unwrap() = true;
    let labels: Vec<String> = st.windows.lock().unwrap().keys().cloned().collect();
    for label in &labels {
        for w in wallpaper_windows(app, label) {
            destroy_wallpaper_window(app, &w);
        }
    }
    tracing::info!(
        "auto-pause (release): 已销毁 {} 块壁纸窗口（渲染进程结束；回桌面时整窗重建）",
        labels.len()
    );
    crate::mem_watch::report("自动暂停释放内存（销毁壁纸窗口）");
}

#[tauri::command(rename = "wallpaper_resume_all")]
pub fn resume_all(app: AppHandle) -> Result<(), String> {
    if let Some(st) = app.try_state::<WallpaperEngineState>() {
        *st.paused.lock().unwrap() = false;
    }
    eval_all(&app, "window.__wp && window.__wp.resume()");
    // 「暂停释放内存」挂起期间的恢复 = 按保留的会话配置整窗重建（完全重新
    // 加载壁纸）。所有恢复路径（回桌面/托盘关开关/应用新壁纸/全局快捷键）
    // 都汇到本函数，重建逻辑放这一处即可全覆盖。
    let rebuild = {
        let Some(st) = app.try_state::<WallpaperEngineState>() else {
            return Ok(());
        };
        let mut g = st.released.lock().unwrap();
        std::mem::replace(&mut *g, false)
    };
    if rebuild {
        let (tx, rx) = std::sync::mpsc::channel();
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            ensure_windows(&app2);
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(Duration::from_secs(5));
        tracing::info!("auto-pause (release): 壁纸窗口已按会话配置重建");
    }
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
    crate::notify_setting_changed(&app, "wallpaper_fit", &fit);
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
    crate::notify_setting_changed(&app, "wallpaper_render_dpr", &format!("{dpr}"));
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

/// 设置全局场景帧率上限（15/24/30/45/60/120），持久化并对所有壁纸窗口实时生效。
#[tauri::command(rename = "wallpaper_set_scene_fps")]
pub fn set_scene_fps(app: AppHandle, fps: u32) -> Result<(), String> {
    if !SCENE_FPS_CHOICES.contains(&fps) {
        return Err(format!(
            "场景帧率仅支持 {}（收到 {fps}）",
            SCENE_FPS_CHOICES
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("/")
        ));
    }
    if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = db::set_setting(&conn, "wallpaper_scene_fps", &fps.to_string());
        }
    }
    crate::notify_setting_changed(&app, "wallpaper_scene_fps", &fps.to_string());
    eval_all(
        &app,
        &format!("window.__wp && window.__wp.setSceneFps({fps})"),
    );
    Ok(())
}

/// 设置全局抗锯齿模式（off/fxaa/msaa2/msaa4，库 1.3.23+），持久化并对所有壁纸窗口实时生效。
#[tauri::command(rename = "wallpaper_set_aa")]
pub fn set_aa(app: AppHandle, mode: String) -> Result<(), String> {
    if !AA_CHOICES.contains(&mode.as_str()) {
        return Err(format!(
            "未知的抗锯齿模式: {mode}（可选 {}）",
            AA_CHOICES.join("/")
        ));
    }
    if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = db::set_setting(&conn, "wallpaper_aa", &mode);
        }
    }
    crate::notify_setting_changed(&app, "wallpaper_aa", &mode);
    tracing::info!("anti-aliasing set: {mode}");
    eval_all(
        &app,
        &format!(
            "window.__wp && window.__wp.setQuality({{antiAliasing:{}}})",
            serde_json::json!(mode)
        ),
    );
    Ok(())
}

/// 设置全局粒子质量档（off/low/medium/high，库 1.3.23+），持久化并对所有壁纸窗口实时生效。
#[tauri::command(rename = "wallpaper_set_particles")]
pub fn set_particles(app: AppHandle, quality: String) -> Result<(), String> {
    if !PARTICLE_QUALITY_CHOICES.contains(&quality.as_str()) {
        return Err(format!(
            "未知的粒子质量档: {quality}（可选 {}）",
            PARTICLE_QUALITY_CHOICES.join("/")
        ));
    }
    if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = db::set_setting(&conn, "wallpaper_particles", &quality);
        }
    }
    crate::notify_setting_changed(&app, "wallpaper_particles", &quality);
    tracing::info!("particle quality set: {quality}");
    eval_all(
        &app,
        &format!(
            "window.__wp && window.__wp.setQuality({{particles:{}}})",
            serde_json::json!(quality)
        ),
    );
    Ok(())
}

/// 设置全局后处理质量档（low/medium/high，库 1.3.23+；不提供 off —— 完全关掉
/// 后处理会让辉光/水波类壁纸失去画面效果），持久化并对所有壁纸窗口实时生效。
#[tauri::command(rename = "wallpaper_set_post")]
pub fn set_post(app: AppHandle, quality: String) -> Result<(), String> {
    if !POST_QUALITY_CHOICES.contains(&quality.as_str()) {
        return Err(format!(
            "未知的后处理质量档: {quality}（可选 {}）",
            POST_QUALITY_CHOICES.join("/")
        ));
    }
    if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = db::set_setting(&conn, "wallpaper_post", &quality);
        }
    }
    crate::notify_setting_changed(&app, "wallpaper_post", &quality);
    tracing::info!("post-processing quality set: {quality}");
    eval_all(
        &app,
        &format!(
            "window.__wp && window.__wp.setQuality({{postProcessing:{}}})",
            serde_json::json!(quality)
        ),
    );
    Ok(())
}

/// 设置全局滤镜（托盘「滤镜效果」子菜单），持久化并对所有桌面壁纸窗口实时生效。
///
/// 与显示模式/清晰度/帧率一样是「热切」：渲染器把白名单 id 翻成 CSS filter
/// 挂在渲染容器上，不重挂壁纸、不重新解析 pkg。**只下发到壁纸窗口**
/// （eval_all 遍历的都是 label = wallpaper-* 的窗口），本地库预览不受影响。
#[tauri::command(rename = "wallpaper_set_filter")]
pub fn set_filter(app: AppHandle, filter: String) -> Result<(), String> {
    if !WALLPAPER_FILTERS.iter().any(|(k, _)| *k == filter) {
        return Err(format!(
            "未知的滤镜: {filter}（可选 {}）",
            WALLPAPER_FILTERS
                .iter()
                .map(|(k, _)| *k)
                .collect::<Vec<_>>()
                .join("/")
        ));
    }
    if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = db::set_setting(&conn, "wallpaper_filter", &filter);
        }
    }
    crate::notify_setting_changed(&app, "wallpaper_filter", &filter);
    tracing::info!("wallpaper filter set: {filter}");
    eval_all(
        &app,
        &format!(
            "window.__wp && window.__wp.setFilter({})",
            serde_json::json!(filter)
        ),
    );
    Ok(())
}

/// 设置「隐藏图标」开关（桌面图标之下/壁纸上方 = 默认；开启后壁纸窗口在桌面图标之上，可接收鼠标/互动）。默认关闭。
#[tauri::command(async, rename = "wallpaper_interactive_set")]
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
                for w in wallpaper_windows(&app2, &label) {
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

/// 切换 WE 官方素材通路 / 自定义素材根后，让所有壁纸窗口按最新配置整页重载。
///
/// 与抗锯齿/滤镜不同，localAssets 只在挂载时读取（库内 installLocalAssets 的
/// eagerDone 按文档缓存），eval 热更无效，只能整页导航。导航必须在主线程执行。
fn renavigate_all_windows(app: &AppHandle) -> Result<(), String> {
    let st = app
        .try_state::<WallpaperEngineState>()
        .ok_or("壁纸引擎未就绪")?;
    let configs: Vec<(String, WallpaperConfig)> =
        st.windows.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (label, cfg) in configs {
        let target = prepare_cfg(app, &cfg);
        let wins = wallpaper_windows(app, &label);
        if wins.is_empty() {
            continue;
        }
        for w in &wins {
            navigate_to_config(app, w, &target)?;
        }
        st.windows.lock().unwrap().insert(label, target);
    }
    Ok(())
}

fn run_on_main_and_wait<F>(app: &AppHandle, f: F) -> Result<(), String>
where
    F: FnOnce(&AppHandle) -> Result<(), String> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let app2 = app.clone();
    app.run_on_main_thread(move || {
        let _ = tx.send(f(&app2));
    })
    .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| format!("壁纸引擎未响应: {e}"))?
}

/// 开关 WE 官方素材通路（设置 `wallpaper_local_assets`，默认开）。
/// 挂载时生效，这里持久化后让现有壁纸窗口整页重载一次。
#[tauri::command(async, rename = "wallpaper_local_assets_set")]
pub fn local_assets_set(app: AppHandle, enabled: bool) -> Result<(), String> {
    {
        let db = app
            .try_state::<Arc<Mutex<Connection>>>()
            .ok_or("DB 未就绪")?;
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::set_setting(
            &conn,
            "wallpaper_local_assets",
            if enabled { "true" } else { "false" },
        )
        .map_err(|e| e.to_string())?;
    }
    run_on_main_and_wait(&app, renavigate_all_windows)
}

/// 设置自定义 WE assets 根目录（其下需直接有 materials/）；传空串清除，回到
/// Steam 库自动探测。路径不含可用素材树时仍允许保存，状态查询会回报 available:false。
#[tauri::command(async, rename = "wallpaper_we_assets_dir_set")]
pub fn we_assets_dir_set(app: AppHandle, dir: String) -> Result<(), String> {
    let dir = dir.trim().to_string();
    if !dir.is_empty() {
        let p = std::path::Path::new(&dir);
        if !p.is_dir() {
            return Err(format!("目录不存在：{dir}"));
        }
        if !p.join("materials").is_dir() {
            return Err("该目录下没有 materials/ 子目录（应选择 Wallpaper Engine 的 assets 根）".into());
        }
    }
    {
        let db = app
            .try_state::<Arc<Mutex<Connection>>>()
            .ok_or("DB 未就绪")?;
        let conn = db.lock().map_err(|e| e.to_string())?;
        if dir.is_empty() {
            let _ = conn.execute("DELETE FROM settings WHERE key = 'wallpaper_we_assets_dir'", []);
        } else {
            db::set_setting(&conn, "wallpaper_we_assets_dir", &dir).map_err(|e| e.to_string())?;
        }
    }
    // 素材索引按根路径缓存，路径变了顺手清掉避免短时读到旧根清单
    run_on_main_and_wait(&app, renavigate_all_windows)
}

/// 官方素材通路状态（设置页用）：开关、素材根是否探测到、根路径、.tex 数量。
#[tauri::command(rename = "wallpaper_local_assets_status")]
pub fn local_assets_status(app: AppHandle) -> Result<serde_json::Value, String> {
    let (enabled, custom) = {
        let db = app
            .try_state::<Arc<Mutex<Connection>>>()
            .ok_or("DB 未就绪")?;
        let conn = db.lock().map_err(|e| e.to_string())?;
        let enabled = db::get_setting(&conn, "wallpaper_local_assets")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true);
        let custom = db::get_setting(&conn, "wallpaper_we_assets_dir");
        (enabled, custom)
    };
    // 关闭时也告诉设置页「本机有没有素材」，方便解释开关为何看不出区别
    let root = crate::we_assets::resolve_root(custom.as_deref());
    let (available, root_str, tex_count) = match &root {
        Some(p) => (true, p.to_string_lossy().to_string(), crate::we_assets::material_names(p).len()),
        None => (false, String::new(), 0),
    };
    Ok(serde_json::json!({
        "enabled": enabled,
        "available": available,
        "root": root_str,
        "customDir": custom.unwrap_or_default(),
        "texCount": tex_count,
    }))
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

/// 查找壁纸目录里第一个 .pkg 文件（场景 pkg 的最后兜底；排序纪律同 find_first_html）。
pub(crate) fn find_first_pkg(dir: &std::path::Path) -> Option<String> {
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
                if ext.eq_ignore_ascii_case("pkg") {
                    if let Ok(rel) = p.strip_prefix(base) {
                        let rel = rel.to_string_lossy().into_owned();
                        let prio = if rel.starts_with("scenes/") {
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

/// 场景壁纸的 pkg 入口（相对壁纸根）。WE 的场景工程主 pkg 命名五花八门，
/// 按优先级逐级回退：
/// ① project.json `file` 直接指向 .pkg（声明即真相）
/// ② `file` 指向 .json（WE 的 GIF 场景模板工程：file=gifscene.json，
///    编译产物是同目录同名 .pkg —— 843532366 就是这种）
/// ③ 默认布局 scenes/scene.pkg / scene.pkg / gifscene.pkg
/// ④ 目录里第一个 .pkg（兜底，命名再不规范也能用）
pub(crate) fn scene_pkg_entry(dir: &std::path::Path) -> Option<String> {
    let declared = project_json_entry(dir);
    if let Some(rel) = declared
        .as_ref()
        .filter(|p| p.to_ascii_lowercase().ends_with(".pkg"))
    {
        return Some(rel.clone());
    }
    if let Some(json_rel) = declared
        .as_ref()
        .filter(|p| p.to_ascii_lowercase().ends_with(".json"))
    {
        // ".json" 是 ASCII 后缀，按字节截断不会切进多字节字符
        let pkg_rel = format!("{}.pkg", &json_rel[..json_rel.len() - 5]);
        if dir.join(&pkg_rel).is_file() {
            return Some(pkg_rel);
        }
    }
    for rel in [
        "scenes/scene.pkg",
        "scene.pkg",
        "gifscene.pkg",
        "scenes/gifscene.pkg",
    ] {
        if dir.join(rel).is_file() {
            return Some(rel.to_string());
        }
    }
    find_first_pkg(dir)
}

/// 解析本地库壁纸文件 → 渲染器配置（src 指向内容服务器媒体 URL）
fn resolve_item_config(app: &AppHandle, item_id: &str) -> Result<WallpaperConfig, String> {
    let db = app
        .try_state::<Arc<Mutex<rusqlite::Connection>>>()
        .ok_or("DB 未就绪")?;
    let (wtype, dir) = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let wtype = conn
            .query_row(
                "SELECT type FROM library_items WHERE item_id = ?1",
                [item_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(|_| "壁纸不在本地库中（请先下载）".to_string())?;
        // 引用模式条目：内容目录是源目录，不是库根
        let root = crate::library::wallpapers_dir(&app)?;
        let dir = crate::library::resolved_item_dir_in(&conn, &root, item_id);
        (wtype, dir)
    };
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
    // project.json 声明的主资源（`file` 字段）优先于目录枚举 —— 很多工程的主文件
    // 命名并不规范（视频不叫 video.mp4、pkg 不叫 scene.pkg），按名字枚举会挑错文件。
    // project_json_entry 已做安全校验（拒绝绝对路径/.. 穿越、要求文件存在）。
    let declared = |exts: &[&str]| -> Option<String> {
        let rel = crate::wallpaper::project_json_entry(&dir)?;
        let lower = rel.to_ascii_lowercase();
        exts.iter()
            .any(|x| lower.ends_with(x))
            .then(|| format!("{media}/{item_id}/{rel}"))
    };

    let mut cfg = match wtype.as_str() {
        "video" => {
            let src = declared(&[".mp4", ".webm", ".mov", ".m4v", ".mkv", ".avi"])
                .or_else(|| find_first(&[".mp4", ".webm", ".mov"]))
                .ok_or("未找到视频文件")?;
            WallpaperConfig {
                r#type: "video".into(),
                src: Some(src),
                ..Default::default()
            }
        }
        "gif" => {
            let src = declared(&[".gif"])
                .or_else(|| find_first(&[".gif"]))
                .ok_or("未找到 GIF 文件")?;
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
            // pkg 入口解析（声明 .pkg / 模板 json 同名 .pkg / 默认布局 / 目录兜底，
            // 见 scene_pkg_entry）：不少工程的主 pkg 命名/位置不标准，
            // 只认 scene.pkg 会误报缺失（843532366 的 gifscene.pkg 就是这类）
            let mut pkg_entry = scene_pkg_entry(&dir);
            if pkg_entry.is_none() {
                // 缺 pkg：先用本地已有的依赖内容补齐；仍缺的自动加入下载队列
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
                pkg_entry = scene_pkg_entry(&dir);
                if pkg_entry.is_none() {
                    return Err("未找到 scene.pkg（依赖内容已合并但仍缺入口文件）".into());
                }
            }
            WallpaperConfig {
                r#type: "scene".into(),
                src: Some(item_id.to_string()),
                scene_pkg: pkg_entry,
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

/// 把本地库条目应用到桌面（解析文件 → 全部显示器，或 `display_id` 指定的单屏）。
/// 内部共用实现：不触碰播放/暂停状态（轮播在后台切换时必须保持自动暂停）。
fn apply_item_inner(
    app: &AppHandle,
    item_id: &str,
    display_id: Option<String>,
) -> Result<(), String> {
    let cfg = resolve_item_config(app, item_id)?;
    let (tx, rx) = std::sync::mpsc::channel();
    let app2 = app.clone();
    let item_id = item_id.to_string();
    let tag = item_id.clone();
    tracing::info!("apply[{tag}]: 派发到主线程（type={}）", cfg.r#type);
    // ⚠️ 本函数必须在**非主线程**上被调用（所有 tauri 命令入口都标了 `async`）。
    // 同步命令运行在主线程的 WebView2 IPC 回调内，此时 run_on_main_thread 会内联
    // 执行闭包，于是「在 WebView2 回调里再建 WebView2」触发 WebView2 线程模型的
    // reentrancy 死锁：Windows 上整个应用卡死在建窗处（macOS 的 WKWebView 无此限制）。
    // 若要在主线程内联调用，命令会阻塞在 rx.recv 上，连消息循环都无法泵送 —— 同样死。
    app.run_on_main_thread(move || {
        let t0 = std::time::Instant::now();
        tracing::info!("apply[{item_id}]: 主线程处理器进入");
        let res = apply_on_main(&app2, display_id, cfg, Some(&item_id));
        tracing::info!(
            "apply[{item_id}]: 主线程处理器返回（{}ms, ok={}）",
            t0.elapsed().as_millis(),
            res.is_ok()
        );
        let _ = tx.send(res);
    })
    .map_err(|e| e.to_string())?;
    // 带上限等待：主线程若卡在建窗/桌面层放置，这里不再无限挂起（至少能返回错误、
    // 让前端给出提示，日志里也能看到「派发了一条 apply 却没有返回」）。
    match rx.recv_timeout(std::time::Duration::from_secs(20)) {
        Ok(r) => r,
        Err(_) => {
            tracing::error!(
                "apply[{tag}]: 主线程处理器 20s 未返回 —— 卡在建窗或桌面层放置，详见上一条日志"
            );
            Err("应用壁纸超时：主线程在创建壁纸窗口时卡住（请把日志发给作者）".into())
        }
    }
}

/// 把本地库条目应用到桌面（用户显式点击；自动暂停态下立即恢复播放）。
/// `display_id` 缺省 = 全部显示器；传值 = 只刷该屏（显示器管理 / 独立模式）。
/// 手动应用 = 该屏回到固定单张：清掉对应轮播绑定（统一上下文不动）。
#[tauri::command(async, rename = "wallpaper_apply_item")]
pub fn apply_item(
    app: AppHandle,
    item_id: String,
    display_id: Option<String>,
) -> Result<(), String> {
    let res = apply_item_inner(&app, &item_id, display_id.clone());
    if res.is_ok() {
        clear_display_bindings(&app, display_id.as_deref());
        resume_if_auto_paused(&app);
    }
    res
}

/// 清掉手动应用所影响的屏的轮播绑定：传值清单屏；缺省清全部屏（统一上下文不动，
/// 全停用 [`playlist_stop`]）。
fn clear_display_bindings(app: &AppHandle, display_id: Option<&str>) {
    let Some(db) = app.try_state::<Arc<Mutex<rusqlite::Connection>>>() else {
        return;
    };
    let Ok(conn) = db.lock() else {
        return;
    };
    match display_id {
        Some(d) => {
            if read_ctx(&conn, d).is_some() {
                clear_ctx(&conn, d);
                clock_forget(d);
            }
        }
        None => {
            for s in platform::active_screens() {
                let key = s.id.to_string();
                if read_ctx(&conn, &key).is_some() {
                    clear_ctx(&conn, &key);
                    clock_forget(&key);
                }
            }
        }
    }
}

/// 绑定/解绑某屏的轮播列表（独立模式的每屏上下文）：
/// - `Some(playlist_id)`：该屏开始轮播该列表（立即应用第一项）；
/// - `None`：解绑，回到固定单张（当前壁纸保持不动）。
#[tauri::command(async, rename = "display_binding_set")]
pub fn display_binding_set(
    app: AppHandle,
    display_id: String,
    playlist_id: Option<i64>,
) -> Result<(), String> {
    // 目标屏必须在（绑到拔掉的屏上没有意义，会话也对不上）
    if !platform::active_screens().iter().any(|s| s.id.to_string() == display_id) {
        return Err("未找到目标显示器".into());
    }
    let Some(pid) = playlist_id else {
        {
            let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
            let conn = db.lock().map_err(|e| e.to_string())?;
            clear_ctx(&conn, &display_id);
        }
        clock_forget(&display_id);
        crate::update_tray_rotation(&app);
        return Ok(());
    };
    let mut p = {
        let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        list_playlists(&conn)?
            .into_iter()
            .find(|p| p.id == pid)
            .ok_or("播放列表不存在")?
    };
    // 剪条目（resolve 回表查库）锁外做，见 load_ctx_pruned 注释
    let gone = prune_items(&app, &mut p);
    {
        let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        if gone > 0 {
            let _ = conn.execute(
                "UPDATE playlists SET item_ids = ?1 WHERE id = ?2",
                rusqlite::params![serde_json::to_string(&p.item_ids).unwrap_or_default(), pid],
            );
        }
        if p.item_ids.is_empty() {
            return Err("播放列表为空".into());
        }
        save_ctx(&conn, &fresh_ctx(&display_id, p.clone(), 0))?;
    }
    clock_reset(&display_id);
    let first = p.item_ids[0].clone();
    apply_item_inner(&app, &first, Some(display_id.clone()))?;
    crate::update_tray_rotation(&app);
    tracing::info!(
        "display {} bound to playlist {} ({} items, {}s, shuffle={})",
        display_id,
        p.id,
        p.item_ids.len(),
        p.interval_sec,
        p.shuffle
    );
    Ok(())
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
    /// 随机播放（洗牌队列：一轮内不重复、可回退）
    #[serde(default)]
    pub shuffle: bool,
}

/// 轮播上下文键：统一模式全局一份（"unified"）；独立模式每块绑定的屏一份（键 = display_id）。
const CTX_UNIFIED: &str = "unified";

/// 轮播上下文 = 列表快照 + 进度（index）+ 洗牌队列。统一/每屏共用同一套读写
/// （settings 四件套，键名见 [`skey`]）——两套语义只差「应用目标屏」，见 [`step_one`]。
#[derive(Debug, Clone)]
struct Ctx {
    key: String,
    playlist: Playlist,
    index: i64,
    /// shuffle 且 n>1 时为 item_ids 下标的完整排列（一轮内不重复、可回退）；否则为空
    queue: Vec<i64>,
    /// 当前项在队列里的位置
    queue_pos: i64,
}

/// 上下文 → settings 键名。统一模式沿用历史键名（active_playlist 等，免迁移）；
/// 每屏上下文用 `rot:<display_id>:<field>`。
fn skey(ctx: &str, field: &str) -> String {
    if ctx == CTX_UNIFIED {
        match field {
            "playlist" => "active_playlist".to_string(),
            "index" => "playlist_index".to_string(),
            "queue" => "playlist_queue".to_string(),
            _ => "playlist_queue_pos".to_string(),
        }
    } else {
        format!("rot:{ctx}:{field}")
    }
}

/// 新上下文（index 处打头；shuffle 时建洗牌队列）。激活列表 / 绑定屏时用。
fn fresh_ctx(key: &str, playlist: Playlist, index: i64) -> Ctx {
    let n = playlist.item_ids.len();
    let (queue, queue_pos) = if playlist.shuffle && n > 1 {
        (queue_with_head(make_queue(n), index), 0)
    } else {
        (Vec::new(), 0)
    };
    Ctx {
        key: key.to_string(),
        playlist,
        index,
        queue,
        queue_pos,
    }
}

/// 读上下文（含 index/队列的合法性收敛）。无激活返回 None。
fn read_ctx(conn: &Connection, key: &str) -> Option<Ctx> {
    let raw = db::get_setting(conn, &skey(key, "playlist"))?;
    if raw.trim().is_empty() {
        return None;
    }
    let playlist: Playlist = serde_json::from_str(&raw).ok()?;
    let n = playlist.item_ids.len();
    if n == 0 {
        return None;
    }
    let mut index: i64 = db::get_setting(conn, &skey(key, "index"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if index < 0 || index >= n as i64 {
        index = 0;
    }
    let (queue, queue_pos) = if playlist.shuffle && n > 1 {
        let q: Option<Vec<i64>> = db::get_setting(conn, &skey(key, "queue"))
            .and_then(|s| serde_json::from_str(&s).ok());
        let pos: Option<i64> = db::get_setting(conn, &skey(key, "qpos"))
            .and_then(|s| s.parse().ok());
        match (q, pos) {
            (Some(q), Some(pos)) if q.len() == n && pos >= 0 && (pos as usize) < n => (q, pos),
            // 缺失/失配（列表变过）：以当前项为队头重建
            _ => (queue_with_head(make_queue(n), index), 0),
        }
    } else {
        (Vec::new(), 0)
    };
    Some(Ctx {
        key: key.to_string(),
        playlist,
        index,
        queue,
        queue_pos,
    })
}

fn save_ctx(conn: &Connection, ctx: &Ctx) -> Result<(), String> {
    db::set_setting(
        conn,
        &skey(&ctx.key, "playlist"),
        &serde_json::to_string(&ctx.playlist).unwrap_or_default(),
    )?;
    db::set_setting(conn, &skey(&ctx.key, "index"), &ctx.index.to_string())?;
    db::set_setting(
        conn,
        &skey(&ctx.key, "queue"),
        &serde_json::to_string(&ctx.queue).unwrap_or_default(),
    )?;
    db::set_setting(conn, &skey(&ctx.key, "qpos"), &ctx.queue_pos.to_string())?;
    Ok(())
}

fn clear_ctx(conn: &Connection, key: &str) {
    for f in ["playlist", "index", "queue", "qpos"] {
        let _ = conn.execute("DELETE FROM settings WHERE key = ?1", [skey(key, f)]);
    }
}

/// 洗牌队列工具（item_ids 下标排列）：
/// - [`make_queue`]：洗出 0..n 的随机排列（Fisher-Yates）；
/// - [`queue_with_head`]：队头换成指定项（激活/重建时从当前项出发）；
/// - [`queue_avoid_head`]：新一轮重洗的排列，队头避开指定项（不与刚播完的连续重复）。
fn make_queue(n: usize) -> Vec<i64> {
    use rand::RngCore;
    let mut v: Vec<i64> = (0..n as i64).collect();
    if n > 1 {
        let mut rng = rand::thread_rng();
        for i in (1..n).rev() {
            let j = (rng.next_u32() as usize) % (i + 1);
            v.swap(i, j);
        }
    }
    v
}

fn queue_with_head(mut q: Vec<i64>, head: i64) -> Vec<i64> {
    if q.len() > 1 {
        if let Some(pos) = q.iter().position(|&x| x == head) {
            q.swap(0, pos);
        }
    }
    q
}

fn queue_avoid_head(mut q: Vec<i64>, avoid: i64) -> Vec<i64> {
    if q.len() > 1 && q[0] == avoid {
        q.swap(0, 1);
    }
    q
}

fn list_playlists(conn: &Connection) -> Result<Vec<Playlist>, String> {
    let mut stmt = conn
        .prepare("SELECT id, name, item_ids, interval_sec, shuffle FROM playlists ORDER BY id")
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
                shuffle: r.get::<_, i64>(4)? != 0,
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
pub fn playlist_get(app: AppHandle, id: i64) -> Result<Playlist, String> {
    let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    list_playlists(&conn)?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| "播放列表不存在".into())
}

#[tauri::command]
pub fn playlist_create(
    app: AppHandle,
    name: String,
    item_ids: Vec<String>,
    interval_sec: i64,
    shuffle: Option<bool>,
) -> Result<i64, String> {
    let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO playlists(name, item_ids, interval_sec, shuffle) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            name,
            serde_json::to_string(&item_ids).unwrap_or_default(),
            interval_sec.max(30),
            shuffle.unwrap_or(false) as i64
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

/// 更新播放列表（None 字段不改）。条目或随机开关变化时重建轮播队列；
/// 若是当前激活列表，同步快照并收敛 index —— 否则改完列表定时器还在按旧快照切。
#[tauri::command(async)]
pub fn playlist_update(
    app: AppHandle,
    id: i64,
    name: Option<String>,
    item_ids: Option<Vec<String>>,
    interval_sec: Option<i64>,
    shuffle: Option<bool>,
) -> Result<Playlist, String> {
    let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let mut all = list_playlists(&conn)?;
    let p = all
        .iter_mut()
        .find(|x| x.id == id)
        .ok_or("播放列表不存在")?;
    let mut shape_changed = false;
    if let Some(n) = name {
        p.name = n;
    }
    if let Some(ids) = item_ids {
        shape_changed = ids != p.item_ids;
        p.item_ids = ids;
    }
    if let Some(iv) = interval_sec {
        p.interval_sec = iv.max(30);
    }
    if let Some(s) = shuffle {
        if s != p.shuffle {
            shape_changed = true;
        }
        p.shuffle = s;
    }
    conn.execute(
        "UPDATE playlists SET name = ?1, item_ids = ?2, interval_sec = ?3, shuffle = ?4,
         updated_at = ?5 WHERE id = ?6",
        rusqlite::params![
            p.name,
            serde_json::to_string(&p.item_ids).unwrap_or_default(),
            p.interval_sec,
            p.shuffle as i64,
            chrono::Utc::now().timestamp(),
            id
        ],
    )
    .map_err(|e| e.to_string())?;
    let updated = p.clone();
    drop(all);

    // 同步所有引用该列表的上下文（统一 + 各绑定屏）：更新快照、收敛 index、按需重建队列
    let mut keys: Vec<String> = vec![CTX_UNIFIED.to_string()];
    keys.extend(platform::active_screens().iter().map(|s| s.id.to_string()));
    for key in keys {
        let Some(mut ctx) = read_ctx(&conn, &key) else {
            continue;
        };
        if ctx.playlist.id != id {
            continue;
        }
        ctx.playlist = updated.clone();
        let n = updated.item_ids.len() as i64;
        if ctx.index < 0 || ctx.index >= n {
            ctx.index = 0;
        }
        if shape_changed {
            let fresh = fresh_ctx(&key, updated.clone(), ctx.index);
            ctx.queue = fresh.queue;
            ctx.queue_pos = fresh.queue_pos;
        }
        save_ctx(&conn, &ctx)?;
    }
    drop(conn);
    clock_clear_all();
    crate::update_tray_rotation(&app);
    Ok(updated)
}

#[tauri::command]
pub fn playlist_delete(app: AppHandle, id: i64) -> Result<bool, String> {
    let was_active = {
        let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM playlists WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
        // 删的是当前激活列表：连轮播状态一起清，否则定时器拿着快照继续切
        let was = db::get_setting(&conn, "active_playlist")
            .and_then(|raw| serde_json::from_str::<Playlist>(&raw).ok())
            .map(|ap| ap.id == id)
            .unwrap_or(false);
        if was {
            let _ = conn.execute(
                "DELETE FROM settings WHERE key IN
                 ('active_playlist', 'playlist_index', 'playlist_queue', 'playlist_queue_pos')",
                [],
            );
        }
        // 绑定到屏的上下文也一并清（删除的列表不该在任何屏上继续轮播）
        for s in platform::active_screens() {
            let key = s.id.to_string();
            if read_ctx(&conn, &key).map(|c| c.playlist.id == id).unwrap_or(false) {
                clear_ctx(&conn, &key);
                clock_forget(&key);
            }
        }
        was
    };
    if was_active {
        // update_tray_rotation → playlist_status 会拿 DB 锁，必须在锁外调
        crate::update_tray_rotation(&app);
    }
    Ok(true)
}

/// 激活播放列表：设置 active_playlist 并立即应用第一项
#[tauri::command(async)]
pub fn playlist_apply(app: AppHandle, id: i64) -> Result<serde_json::Value, String> {
    let mut playlist = {
        let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        list_playlists(&conn)?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or("播放列表不存在")?
    };
    // 剪掉失效条目（文件已丢失等）——resolve 回表查库，锁外做（见 load_active 注释）
    let gone = prune_items(&app, &mut playlist);
    {
        let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        if gone > 0 {
            tracing::info!("playlist {}: {} 个条目已失效，已从列表移除", playlist.name, gone);
            let _ = conn.execute(
                "UPDATE playlists SET item_ids = ?1 WHERE id = ?2",
                rusqlite::params![
                    serde_json::to_string(&playlist.item_ids).unwrap_or_default(),
                    id
                ],
            );
        }
        if playlist.item_ids.is_empty() {
            return Err("播放列表为空".into());
        }
        db::set_setting(
            &conn,
            "active_playlist",
            &serde_json::to_string(&playlist).unwrap_or_default(),
        )?;
        db::set_setting(&conn, "playlist_index", "0")?;
        save_ctx(&conn, &fresh_ctx(CTX_UNIFIED, playlist.clone(), 0))?;
    }
    // 应用第一项（失败不回滚激活态：条目在，只是当前应用失败，可下一张重试）
    let first = playlist.item_ids[0].clone();
    apply_item(app.clone(), first, None)?;
    clock_reset(CTX_UNIFIED);
    crate::update_tray_rotation(&app);
    tracing::info!(
        "playlist {} activated ({} items, {}s, shuffle={})",
        playlist.name,
        playlist.item_ids.len(),
        playlist.interval_sec,
        playlist.shuffle
    );
    Ok(serde_json::to_value(&playlist).unwrap_or_default())
}

/// 下一张（手动或轮播定时）。`display_id` = 只切该屏（独立模式）；缺省按当前模式
/// 的活跃上下文走（统一 = 全局列表；独立 = 各绑定屏各自前进一张）。
#[tauri::command(async, rename = "wallpaper_next")]
pub fn next(app: AppHandle, display_id: Option<String>) -> Result<serde_json::Value, String> {
    step_cmd(app, true, display_id)
}

/// 上一张（顺序模式 ±1 环形；随机模式沿洗牌队列回退）
#[tauri::command(async, rename = "wallpaper_prev")]
pub fn prev(app: AppHandle, display_id: Option<String>) -> Result<serde_json::Value, String> {
    step_cmd(app, false, display_id)
}

fn step_cmd(
    app: AppHandle,
    forward: bool,
    display_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let keys: Vec<String> = match &display_id {
        Some(d) => vec![d.clone()],
        None => {
            let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
            let conn = db.lock().map_err(|e| e.to_string())?;
            active_ctx_keys(&conn)
        }
    };
    if keys.is_empty() {
        return Err("未激活播放列表".into());
    }
    let mut steps = Vec::new();
    let mut last_err = None;
    for key in &keys {
        match step_one(&app, key, forward) {
            Ok((item, index)) => steps.push(serde_json::json!({
                "displayId": (*key != CTX_UNIFIED).then(|| key.clone()),
                "itemId": item,
                "index": index,
            })),
            Err(e) => {
                tracing::debug!("step[{key}]: {e}");
                last_err = Some(e);
            }
        }
    }
    if steps.is_empty() {
        return Err(last_err.unwrap_or_else(|| "未激活播放列表".into()));
    }
    crate::update_tray_rotation(&app);
    // 单步时保持旧返回形（itemId / index）；多步（独立模式逐屏）时附完整 steps
    let first = &steps[0];
    Ok(serde_json::json!({
        "itemId": first["itemId"],
        "index": first["index"],
        "steps": steps,
    }))
}

/// 一个上下文走一步并应用：统一 = 刷全部屏；每屏 = 只刷那块屏。
/// 手动/自动切换走同一入口：都重置该上下文的时钟（刚切过就重新计时）。
fn step_one(app: &AppHandle, key: &str, forward: bool) -> Result<(String, i64), String> {
    let mut ctx = load_ctx_pruned(app, key)?.ok_or("未激活播放列表")?;
    let to = {
        let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        let to = step_ctx(&mut ctx, forward);
        ctx.index = to;
        save_ctx(&conn, &ctx)?;
        to
    };
    let item = ctx.playlist.item_ids[to as usize].clone();
    clock_reset(key);
    let display = (key != CTX_UNIFIED).then(|| key.to_string());
    apply_item_inner(app, &item, display)?;
    Ok((item, to))
}

/// 轮播暂停开关（持久化）。暂停的是「定时自动切换」，壁纸渲染不受影响
/// （那是 ⌘⇧P 的「暂停播放」，两回事）。
#[tauri::command(async, rename = "wallpaper_rotation_set")]
pub fn rotation_set(app: AppHandle, paused: bool) -> Result<(), String> {
    {
        let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::set_setting(
            &conn,
            "playlist_rotation_paused",
            if paused { "true" } else { "false" },
        )?;
    }
    // 暂停/恢复都重新计时：恢复后满间隔再切，不补切暂停期间积压的次数
    clock_clear_all();
    crate::update_tray_rotation(&app);
    // 托盘「轮播」子菜单与本地库轮播条的暂停钮要互相跟上（托盘点击时 UI 侧
    // 没有自己的动作可挂靠，只能靠这条广播）
    crate::notify_setting_changed(
        &app,
        "playlist_rotation_paused",
        if paused { "true" } else { "false" },
    );
    Ok(())
}

/// 停止轮播（清除激活状态；壁纸停在当前这张不动）
#[tauri::command(async, rename = "playlist_stop")]
pub fn playlist_stop(app: AppHandle) -> Result<(), String> {
    {
        let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        let _ = conn.execute(
            "DELETE FROM settings WHERE key IN
             ('active_playlist', 'playlist_index', 'playlist_queue', 'playlist_queue_pos')
             OR key LIKE 'rot:%'",
            [],
        );
    }
    clock_clear_all();
    crate::update_tray_rotation(&app);
    Ok(())
}

/// 轮播状态（切换列表页 / 显示器页 / 托盘共用）：
/// 无激活列表时 `active: false`；倒计时以 nextAtMs 给出（暂停中为 null）。
#[tauri::command(async, rename = "playlist_status")]
pub fn playlist_status(app: AppHandle) -> Result<serde_json::Value, String> {
    let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let paused = rotation_paused(&conn);
    let Some(raw) = db::get_setting(&conn, "active_playlist") else {
        return Ok(serde_json::json!({ "active": false, "paused": paused }));
    };
    let Ok(p) = serde_json::from_str::<Playlist>(&raw) else {
        return Ok(serde_json::json!({ "active": false, "paused": paused }));
    };
    let index: i64 = db::get_setting(&conn, "playlist_index")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let interval = p.interval_sec.max(30);
    let last = clock_get(CTX_UNIFIED);
    let next_at_ms = if paused || last <= 0 {
        None
    } else {
        Some((last + interval) * 1000)
    };
    let mode = db::get_setting(&conn, "display_mode").unwrap_or_else(|| "unified".to_string());
    Ok(serde_json::json!({
        "active": true,
        "paused": paused,
        "mode": mode,
        "id": p.id,
        "name": p.name,
        "shuffle": p.shuffle,
        "index": index,
        "total": p.item_ids.len(),
        "intervalSec": interval,
        "nextAtMs": next_at_ms,
    }))
}

// ---------- 轮播内部：时钟 / 条目清理 / 走步 ----------

/// 轮播时钟（unix 秒）：每个上下文一份（键同 [`skey`]）。
/// 手动/自动切换与暂停开关都会重置对应上下文 —— 「刚手动切过就重新计时」。
/// 缺失/0 = 未初始化（应用重启后由定时任务补基准，间隔从启动起算）。
static ROT_CLOCKS: Mutex<Vec<(String, i64)>> = Mutex::new(Vec::new());

fn clock_reset(key: &str) {
    if let Ok(mut g) = ROT_CLOCKS.lock() {
        let now = chrono::Utc::now().timestamp();
        match g.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = now,
            None => g.push((key.to_string(), now)),
        }
    }
}

fn clock_get(key: &str) -> i64 {
    ROT_CLOCKS
        .lock()
        .ok()
        .and_then(|g| g.iter().find(|(k, _)| k == key).map(|(_, v)| *v))
        .unwrap_or(0)
}

fn clock_forget(key: &str) {
    if let Ok(mut g) = ROT_CLOCKS.lock() {
        g.retain(|(k, _)| k != key);
    }
}

fn clock_clear_all() {
    if let Ok(mut g) = ROT_CLOCKS.lock() {
        g.clear();
    }
}

/// 仅充电时轮播（设置开）：电池供电时暂缓自动切换（手动切换不受影响）。
fn rotation_power_blocked(conn: &Connection) -> bool {
    let on = matches!(
        db::get_setting(conn, "playlist_rotation_power").as_deref(),
        Some("true") | Some("1")
    );
    on && platform::on_ac_power() == Some(false)
}

/// 当前模式下活跃的上下文键：统一 = ["unified"]；独立 = 已绑定列表的屏 id。
fn active_ctx_keys(conn: &Connection) -> Vec<String> {
    let mode = db::get_setting(conn, "display_mode").unwrap_or_else(|| "unified".to_string());
    if mode != "independent" {
        return vec![CTX_UNIFIED.to_string()];
    }
    platform::active_screens()
        .iter()
        .map(|s| s.id.to_string())
        .filter(|id| read_ctx(conn, id).is_some())
        .collect()
}

/// 轮播暂停（持久化设置）。暂停的是「自动切换」，不是壁纸渲染。
fn rotation_paused(conn: &Connection) -> bool {
    matches!(
        db::get_setting(conn, "playlist_rotation_paused").as_deref(),
        Some("true") | Some("1")
    )
}

/// 剪掉失效条目（本地文件已丢失等，配置解析失败）。返回剪掉数量。
fn prune_items(app: &AppHandle, p: &mut Playlist) -> usize {
    let before = p.item_ids.len();
    p.item_ids.retain(|id| resolve_item_config(app, id).is_ok());
    before - p.item_ids.len()
}

/// 读上下文并剪掉失效条目（快照 + 源表写回）。无激活返回 None。
///
/// ⚠️ 剪条目调 resolve_item_config（内部回表查库）——必须在**不持有 DB 锁**时做，
/// 否则同一把 `Mutex<Connection>` 自锁死（本模块所有「锁内不做解析」都源于此）。
fn load_ctx_pruned(app: &AppHandle, key: &str) -> Result<Option<Ctx>, String> {
    let mut ctx = {
        let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        match read_ctx(&conn, key) {
            Some(c) => c,
            None => return Ok(None),
        }
    };
    let gone = prune_items(app, &mut ctx.playlist);
    if gone > 0 {
        tracing::info!(
            "playlist {}: {} 个条目已失效，已跳过并移除",
            ctx.playlist.name,
            gone
        );
        let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        if ctx.playlist.item_ids.is_empty() {
            clear_ctx(&conn, key);
            clock_forget(key);
            return Ok(None);
        }
        let n = ctx.playlist.item_ids.len() as i64;
        if ctx.index >= n {
            ctx.index = 0;
        }
        if ctx.playlist.shuffle && n > 1 {
            ctx.queue = queue_with_head(make_queue(n as usize), ctx.index);
            ctx.queue_pos = 0;
        }
        save_ctx(&conn, &ctx)?;
        // 源表同步（列表实体可能还在；已删除则 UPDATE 不命中，无副作用）
        let _ = conn.execute(
            "UPDATE playlists SET item_ids = ?1 WHERE id = ?2",
            rusqlite::params![
                serde_json::to_string(&ctx.playlist.item_ids).unwrap_or_default(),
                ctx.playlist.id
            ],
        );
    }
    Ok(Some(ctx))
}

/// 走一步（forward/backward）：顺序 ±1 环形；随机沿洗牌队列（一轮内不重复、可回退，
/// 走穿队尾重洗且队头避开当前项）。纯函数，队列/位置写回 ctx，index 由调用方落。
fn step_ctx(ctx: &mut Ctx, forward: bool) -> i64 {
    let n = ctx.playlist.item_ids.len() as i64;
    if n <= 1 {
        return 0;
    }
    if !ctx.playlist.shuffle {
        return if forward {
            (ctx.index + 1) % n
        } else {
            (ctx.index - 1 + n) % n
        };
    }
    let mut pos = ctx.queue_pos;
    if forward {
        pos += 1;
        if pos >= n {
            ctx.queue = queue_avoid_head(make_queue(n as usize), ctx.index);
            pos = 0;
        }
    } else {
        pos -= 1;
        if pos < 0 {
            pos = n - 1; // 环到本队列队尾（回退语义：本轮最后那格）
        }
    }
    ctx.queue_pos = pos;
    ctx.queue[pos as usize]
}

/// 洗牌队列（item_ids 下标的完整排列，随 [`Ctx`] 存放）：
/// - 随机模式沿队列 ±1（回退 = 回到本队列上一个，一轮内不重复）；走穿队尾重洗一轮
///   （新队列头避开当前项，不连续重复）。
/// - 顺序模式不用队列，index 直接 ±1 环形。见 [`step_ctx`]。

/// 轮播定时任务：按间隔自动下一张（由 init 启动），按模式驱动活跃上下文
/// （统一一份 / 独立每屏一份，各自计时）。手动切换会重置对应时钟（[`clock_reset`]）；
/// 暂停轮播与「仅充电时轮播」的电池暂缓期间不动。时钟缺失（应用刚启动）只补基准
/// 不切换 —— 间隔从启动起算。
pub fn start_playlist_rotation(app: &AppHandle) {
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let due: Vec<String> = {
                let Some(db) = app2.try_state::<Arc<Mutex<rusqlite::Connection>>>() else {
                    continue;
                };
                let conn = match db.lock() {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if rotation_paused(&conn) || rotation_power_blocked(&conn) {
                    continue;
                }
                let mut due = Vec::new();
                for key in active_ctx_keys(&conn) {
                    let Some(ctx) = read_ctx(&conn, &key) else {
                        continue;
                    };
                    let interval = ctx.playlist.interval_sec.max(30);
                    let last = clock_get(&key);
                    if last <= 0 {
                        clock_reset(&key); // 重启后补基准：间隔从启动起算
                    } else if chrono::Utc::now().timestamp() - last >= interval {
                        due.push(key);
                    }
                }
                due
            };
            for key in due {
                match step_one(&app2, &key, true) {
                    Ok(_) => crate::update_tray_rotation(&app2),
                    Err(e) => tracing::debug!("playlist rotation[{key}]: {e}"),
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

    /// 轮播走步：顺序模式 ±1 环形；随机模式沿洗牌队列（一轮内不重复、可回退，
    /// 跨轮重洗后头一张不与上一张连续重复）。
    #[test]
    fn playlist_step_sequential_and_shuffle() {
        let mut ctx = Ctx {
            key: "t".into(),
            playlist: Playlist {
                id: 1,
                name: "t".into(),
                item_ids: (0..5).map(|i| format!("i{i}")).collect(),
                interval_sec: 60,
                shuffle: false,
            },
            index: 0,
            queue: Vec::new(),
            queue_pos: 0,
        };
        // 顺序：±1 环形
        ctx.index = 0;
        assert_eq!(step_ctx(&mut ctx, true), 1);
        ctx.index = 2;
        assert_eq!(step_ctx(&mut ctx, true), 3);
        ctx.index = 2;
        assert_eq!(step_ctx(&mut ctx, false), 1);
        ctx.index = 0;
        assert_eq!(step_ctx(&mut ctx, false), 4);

        // 随机：洗牌队列一轮内不重复
        ctx.playlist.shuffle = true;
        ctx.index = 0;
        ctx.queue = queue_with_head(make_queue(5), 0);
        ctx.queue_pos = 0;
        let mut seen = vec![0i64];
        for _ in 0..4 {
            let to = step_ctx(&mut ctx, true);
            assert!(!seen.contains(&to), "一轮内重复了: {seen:?} → {to}");
            seen.push(to);
            ctx.index = to;
        }
        // 回退两步再前进两步：队列位置 ±1，原路返回
        let cur = ctx.index;
        let back1 = step_ctx(&mut ctx, false);
        ctx.index = back1;
        let back2 = step_ctx(&mut ctx, false);
        ctx.index = back2;
        assert_eq!(step_ctx(&mut ctx, true), back1);
        ctx.index = back1;
        assert_eq!(step_ctx(&mut ctx, true), cur);
        ctx.index = cur;
        // 走穿队尾：重洗一轮，头一张不与上一张连续重复
        let fresh = step_ctx(&mut ctx, true);
        assert_ne!(fresh, cur, "跨轮不应与上一张连续重复");
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

    /// 「同一张壁纸」判定：决定切换时是热更新（同一文档换实例）还是整页换页。
    #[test]
    fn same_wallpaper_ignores_token_and_detects_switch() {
        let mk = |t: &str, src: &str| WallpaperConfig {
            r#type: t.into(),
            src: Some(src.into()),
            ..Default::default()
        };
        // 同一条目、token 刷新（src 变了）→ 仍算同一张，热更新即可
        assert!(same_wallpaper(
            &mk("web", "http://127.0.0.1:1/web/oldtok/3406740580/index.html"),
            &mk("web", "http://127.0.0.1:1/web/newtok/3406740580/index.html"),
        ));
        // scene 的 src 就是 item_id
        assert!(same_wallpaper(&mk("scene", "42"), &mk("scene", "42")));
        assert!(!same_wallpaper(&mk("scene", "42"), &mk("scene", "43")));
        // 类型不同必重建（否则容器/渲染路径不对）
        assert!(!same_wallpaper(
            &mk("scene", "42"),
            &mk("video", "http://127.0.0.1:1/media/tok/42/a.mp4")
        ));
        // item 解析不出时退回比 src
        assert!(same_wallpaper(
            &mk("web", "https://example.com/a.html"),
            &mk("web", "https://example.com/a.html")
        ));
        assert!(!same_wallpaper(
            &mk("web", "https://example.com/a.html"),
            &mk("web", "https://example.com/b.html")
        ));
    }

    /// 渲染器 URL 的契约：换壁纸走的就是「导航到这个 URL」，所以**渲染器的
    /// 全部配置都必须落在 query 上**（渲染器只读 query，见 initialCfg）。
    /// 少一个字段 = 换了壁纸但设置没跟着换，而建窗与换页两条路径必须一致。
    #[test]
    fn renderer_url_carries_every_config_field() {
        let cfg = WallpaperConfig {
            r#type: "scene".into(),
            src: Some("3781035191".into()),
            fit: "contain".into(),
            render_dpr: 1.0,
            scene_fps: 45,
            filter: "blur".into(),
            aa: "fxaa".into(),
            particles: "low".into(),
            post_processing: "medium".into(),
            muted: false,
            volume: Some(0.35),
            r#loop: true,
            scene_pkg: Some("scenes/my.pkg".into()),
            local_assets: true,
            media_base: Some("http://127.0.0.1:1/media/tok".into()),
        };
        let url = renderer_url_on("http://127.0.0.1:57810", &cfg, Some("audiotok")).unwrap();
        assert_eq!(
            url[..url::Position::BeforePath].to_string(),
            "http://127.0.0.1:57810",
            "同源：渲染器页与媒体同源才能免跨源 fetch"
        );
        assert_eq!(url.path(), "/renderer/index.html");
        let q = url.query().unwrap();
        for expect in [
            "type=scene",
            "src=3781035191",
            "fit=contain",
            "renderDpr=1",
            "sceneFps=45",
            "filter=blur",
            // 渲染质量档位（库 1.3.23+）：query 键 aa/pq/pp 与上游 bench 约定一致
            "aa=fxaa",
            "pq=low",
            "pp=medium",
            "muted=false",
            // 精确音量（0..1）：渲染器挂载时先静音，加载完成后按它起音量
            "volume=0.35",
            "loop=true",
            // WE 官方素材通路（库 1.4.1+）：生产构建必须显式带参库才会探测
            "localAssets=1",
            // project.json 声明的场景 pkg 入口（非默认命名时给渲染器指路）
            "scenePkg=scenes%2Fmy.pkg",
            "mediaBase=http%3A%2F%2F127.0.0.1%3A1%2Fmedia%2Ftok",
            "audioToken=audiotok",
        ] {
            assert!(q.split('&').any(|p| p == expect), "query 缺 {expect}：{q}");
        }
    }

    /// macOS：数据存储标识必须每次都不一样。同 label 的窗口销毁重建时若复用同一
    /// 份存储，删掉它就等于把新窗口的存储一起删了（或者删不掉旧进程），回收逻辑
    /// 全靠「一份窗口一份标识」。
    #[cfg(target_os = "macos")]
    #[test]
    fn data_store_ids_are_unique() {
        let ids: std::collections::HashSet<[u8; 16]> =
            (0..64).map(|_| new_data_store_id()).collect();
        assert_eq!(ids.len(), 64, "数据存储标识重复了");
        assert!(!ids.contains(&[0u8; 16]), "全零标识会被 WebKit 判为非法");
    }

    fn fixture(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wpem-entry-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 场景 pkg 入口解析（843532366 的 gifscene 工程就是 ② 那类）
    #[test]
    fn scene_pkg_entry_prefers_declared_then_known_layouts() {
        // ① file 直接指向 .pkg
        let d = fixture("pkg-declared");
        std::fs::write(d.join("my.pkg"), b"p").unwrap();
        std::fs::write(d.join("project.json"), r#"{"file":"my.pkg"}"#).unwrap();
        assert_eq!(scene_pkg_entry(&d).as_deref(), Some("my.pkg"));

        // ② file 指向模板 json（GIF 场景工程）→ 同目录同名 .pkg
        let d = fixture("pkg-gifscene");
        std::fs::write(d.join("gifscene.pkg"), b"p").unwrap();
        std::fs::write(
            d.join("project.json"),
            r#"{"description":"x","file":"gifscene.json"}"#,
        )
        .unwrap();
        assert_eq!(scene_pkg_entry(&d).as_deref(), Some("gifscene.pkg"));

        // ③ 默认布局（含子目录）
        let d = fixture("pkg-default");
        std::fs::create_dir_all(d.join("scenes")).unwrap();
        std::fs::write(d.join("scenes/scene.pkg"), b"p").unwrap();
        assert_eq!(scene_pkg_entry(&d).as_deref(), Some("scenes/scene.pkg"));

        // ④ 兜底：目录里第一个 .pkg（无 project.json 也能找到）
        let d = fixture("pkg-fallback");
        std::fs::write(d.join("weird-name.pkg"), b"p").unwrap();
        std::fs::write(d.join("preview.jpg"), b"i").unwrap();
        assert_eq!(scene_pkg_entry(&d).as_deref(), Some("weird-name.pkg"));

        // 什么都没有 → None
        let d = fixture("pkg-none");
        std::fs::write(d.join("preview.jpg"), b"i").unwrap();
        assert_eq!(scene_pkg_entry(&d), None);
    }

    #[test]
    fn project_json_entry_priority_and_safety() {        // 声明的入口存在 → 采用（该壁纸没有任何 index.html）
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
