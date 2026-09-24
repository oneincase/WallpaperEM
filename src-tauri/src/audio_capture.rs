//! 系统全局音频捕获门面：系统输出 loopback → 64 段频谱（f32 0..1，30Hz）。
//!
//! 采集 / 降采样 / FFT 全部由 media-bridge crate 完成（macOS CoreAudio 进程
//! Tap、Windows WASAPI loopback、Linux parec/PipeWire monitor 三平台同一套），
//! 本模块只做三件事：
//! 1. 按设置开关懒启动 / 停止 bridge 采集（首次启动触发系统授权）；
//! 2. 监控线程把 bridge 的 SourceStatus 翻译成旧的相位机（IDLE/STARTING/
//!    RUNNING/FAILED），并把 u8 0..255 频谱帧经 AGC 后处理写回 [`AudioShared`]；
//! 3. 保持旧的对外契约：两个 Tauri 命令、`wallpaper_audio_processing` 设置键、
//!    `wait_until_ready` 启动排序、content_server SSE / 看门狗接口都不变。

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use media_bridge::types::SourceState;
use serde::Serialize;
use tauri::Manager;

use crate::db;

pub const BANDS: usize = 64;

/// 捕获生命周期相位（AudioShared.phase）
pub const PHASE_IDLE: u8 = 0; // 未启动（开关关闭或已停止）
pub const PHASE_STARTING: u8 = 1; // 启动中（Tap / WASAPI / 采集进程建立期间）
pub const PHASE_RUNNING: u8 = 2; // 运行中（频谱帧持续到达）
pub const PHASE_FAILED: u8 = 3; // 本次启动失败（权限缺失/无采集后端等）

/// 监控线程节拍（≈30Hz，与 SSE 推送节奏一致）
const MONITOR_INTERVAL: Duration = Duration::from_millis(33);

/// 最新频谱帧（内容服务器 SSE 读取；监控线程写入）
pub struct AudioShared {
    /// 捕获生命周期相位（PHASE_*）
    phase: AtomicU8,
    /// 是否曾经进入过运行态（看门狗只对「曾正常工作后卡死」的会话重启，
    /// 避免对从未出帧的环境做无意义的反复重启）
    pub ever_received: AtomicBool,
    pub seq: AtomicU64,
    bands: Mutex<[f32; BANDS]>,
}

impl AudioShared {
    pub(crate) fn new() -> Self {
        Self {
            phase: AtomicU8::new(PHASE_IDLE),
            ever_received: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            bands: Mutex::new([0.0; BANDS]),
        }
    }

    pub fn is_running(&self) -> bool {
        self.phase() == PHASE_RUNNING
    }

    pub fn phase(&self) -> u8 {
        self.phase.load(Ordering::Relaxed)
    }

    pub(crate) fn set_phase(&self, phase: u8) {
        self.phase.store(phase, Ordering::Relaxed);
    }

    /// 读取最新频谱快照（seq 用于 SSE 判断变化）
    pub fn snapshot(&self) -> (u64, [f32; BANDS]) {
        let bands = self.bands.lock().map(|b| *b).unwrap_or([0.0; BANDS]);
        (self.seq.load(Ordering::Relaxed), bands)
    }

    /// 写入新频谱帧
    pub(crate) fn publish(&self, bands: [f32; BANDS]) {
        if let Ok(mut b) = self.bands.lock() {
            *b = bands;
        }
        self.seq.fetch_add(1, Ordering::Relaxed);
    }
}

/// 门面内部状态（监控线程与启停命令共享）
struct Inner {
    shared: Arc<AudioShared>,
    /// 开关期望态：true=该采集（设置开启），false=该释放
    desired: AtomicBool,
    /// AGC / 峰值保持后处理（状态量，停用时复位）
    post: Mutex<spectrum::BandsPost>,
}

static INNER: OnceLock<Arc<Inner>> = OnceLock::new();

fn inner() -> Option<Arc<Inner>> {
    INNER.get().cloned()
}

/// Tauri managed state（content_server 从这里取 shared）
pub struct AudioCaptureState {
    pub shared: Arc<AudioShared>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStatus {
    /// 设置开关（持久化）
    pub enabled: bool,
    /// 捕获是否运行中
    pub running: bool,
    /// 音频捕获权限是否已授予
    pub granted: bool,
    /// 当前平台是否支持系统音频捕获
    pub supported: bool,
}

/// 三平台都由 media-bridge 提供采集实现
const fn platform_supported() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    ))
}

pub fn init(app: &tauri::AppHandle) -> Result<(), String> {
    let shared = Arc::new(AudioShared::new());
    let arc = Arc::new(Inner {
        shared: shared.clone(),
        desired: AtomicBool::new(false),
        post: Mutex::new(spectrum::BandsPost::new()),
    });
    if INNER.set(arc.clone()).is_err() {
        // 重复 init（测试场景）：沿用已有的即可
        return Ok(());
    }
    app.manage(AudioCaptureState {
        shared: shared.clone(),
    });
    spawn_monitor(arc);
    Ok(())
}

/// 监控线程：bridge 状态 → 相位机；bridge 频谱帧 → AGC 后处理 → AudioShared。
///
/// 只在 bridge 帧时间戳变化时 publish：采集后端真的死掉时 bridge 不再产帧，
/// seq 会冻结 —— 看门狗依赖这个信号判断「RUNNING 但卡死」并重启。
fn spawn_monitor(inner: Arc<Inner>) {
    thread::Builder::new()
        .name("wpem-audio-monitor".to_string())
        .spawn(move || {
            let mut last_ts: u64 = 0;
            loop {
                thread::sleep(MONITOR_INTERVAL);
                let Some(bridge) = crate::media_bridge::bridge() else {
                    continue;
                };
                if !inner.desired.load(Ordering::Relaxed) {
                    if inner.shared.phase() != PHASE_IDLE {
                        inner.shared.set_phase(PHASE_IDLE);
                    }
                    continue;
                }

                let state = bridge.audio_status().state;
                match state {
                    SourceState::Running => {
                        inner.shared.ever_received.store(true, Ordering::Relaxed);
                        if inner.shared.phase() != PHASE_RUNNING {
                            inner.shared.set_phase(PHASE_RUNNING);
                        }
                    }
                    SourceState::Preparing => inner.shared.set_phase(PHASE_STARTING),
                    SourceState::Denied | SourceState::Unavailable => {
                        inner.shared.set_phase(PHASE_FAILED)
                    }
                    // 采集中的瞬时错误（如单次取包失败）：后端通常自行重开，
                    // 别误判成本次启动失败；从未跑起来过才算 FAILED
                    SourceState::Error => {
                        if inner.shared.ever_received.load(Ordering::Relaxed) {
                            inner.shared.set_phase(PHASE_RUNNING);
                        } else {
                            inner.shared.set_phase(PHASE_FAILED);
                        }
                    }
                    // ensure_audio 已调、平台后端尚未翻状态：保持 STARTING
                    SourceState::Idle => {
                        if inner.shared.ever_received.load(Ordering::Relaxed) {
                            inner.shared.set_phase(PHASE_RUNNING);
                        } else if inner.shared.phase() == PHASE_IDLE
                            || inner.shared.phase() == PHASE_FAILED
                        {
                            inner.shared.set_phase(PHASE_STARTING);
                        }
                    }
                }

                let frame = bridge.spectrum();
                if frame.ts_ms == last_ts {
                    continue;
                }
                last_ts = frame.ts_ms;
                let mut bands = [0.0f32; BANDS];
                if let Ok(mut post) = inner.post.lock() {
                    post.process(&frame.bands, &mut bands);
                }
                inner.shared.publish(bands);
            }
        })
        .ok();
}

/// 开启捕获（幂等）。未授权时触发系统授权提示；用户当场同意则直接工作，
/// 否则报错（授权后重开开关即可，无需重启应用）。
pub async fn start(_app: tauri::AppHandle) -> Result<(), String> {
    let inner = inner().ok_or("音频捕获状态未就绪")?;
    inner.desired.store(true, Ordering::Relaxed);
    if inner.shared.phase() == PHASE_IDLE || inner.shared.phase() == PHASE_FAILED {
        inner.shared.set_phase(PHASE_STARTING);
    }
    let bridge = crate::media_bridge::bridge().ok_or("媒体桥接未初始化")?;
    match tauri::async_runtime::spawn_blocking(move || bridge.ensure_audio()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            inner.shared.set_phase(PHASE_FAILED);
            Err(e.to_string())
        }
        Err(e) => {
            inner.shared.set_phase(PHASE_FAILED);
            Err(format!("音频捕获线程失败: {e}"))
        }
    }
}

pub async fn stop(_app: tauri::AppHandle) -> Result<(), String> {
    let Some(inner) = inner() else {
        return Ok(());
    };
    inner.desired.store(false, Ordering::Relaxed);
    inner.shared.set_phase(PHASE_IDLE);
    if let Ok(mut post) = inner.post.lock() {
        post.reset();
    }
    // 发一帧静音，立刻结束 SSE 侧的最后画面（随后端点因非 RUNNING 回 503）
    inner.shared.publish([0.0; BANDS]);
    if let Some(bridge) = crate::media_bridge::bridge() {
        tauri::async_runtime::spawn_blocking(move || bridge.stop_audio())
            .await
            .map_err(|e| format!("停止音频线程失败: {e}"))?;
    }
    Ok(())
}

/// macOS 上查询采集授权态（看门狗「授权后自动重试」用）。
#[cfg(target_os = "macos")]
pub fn macos_permission_granted() -> bool {
    let Some(bridge) = crate::media_bridge::bridge() else {
        return false;
    };
    if bridge.audio_status().state == SourceState::Running {
        return true;
    }
    // Idle = 还没申请过（等同系统的 notDetermined）→ false；Denied = 明确拒绝。
    // 其余状态（准备中/瞬时错误/系统不支持）不按「未授权」处理
    !matches!(
        bridge.audio_status().state,
        SourceState::Idle | SourceState::Denied
    )
}

pub fn status(app: &tauri::AppHandle) -> AudioStatus {
    let enabled = app
        .try_state::<Arc<Mutex<rusqlite::Connection>>>()
        .and_then(|db| {
            let conn = db.lock().ok()?;
            Some(
                db::get_setting(&conn, "wallpaper_audio_processing")
                    .map(|v| v == "true")
                    .unwrap_or(true), // 缺省开启；只有用户显式关过（存了 "false"）才关
            )
        })
        .unwrap_or(false); // DB 未就绪（启动极早期）：保守报关，status 随后会再被读
    let running = inner()
        .map(|i| i.shared.is_running())
        .unwrap_or(false);
    let granted = if cfg!(target_os = "macos") {
        running || macos_permission_granted()
    } else {
        // Windows / Linux 采集无需系统级授权
        platform_supported()
    };
    AudioStatus {
        enabled,
        running,
        granted,
        supported: platform_supported(),
    }
}

/// 设置开关并启停捕获（设置页调用）；返回最新状态
pub async fn set_enabled(app: tauri::AppHandle, enabled: bool) -> Result<AudioStatus, String> {
    {
        let db = app
            .try_state::<Arc<Mutex<rusqlite::Connection>>>()
            .ok_or("DB 未就绪")?;
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::set_setting(
            &conn,
            "wallpaper_audio_processing",
            if enabled { "true" } else { "false" },
        )?;
    }
    if enabled {
        // 失败（无采集后端 / 拒绝授权）要把错误返回给设置页提示；开关值已落库，
        // 看门狗会在授权恢复后自动重试
        start(app.clone()).await?;
    } else {
        stop(app.clone()).await?;
    }
    // 通知已挂载的壁纸热切换音频源。刻意不重挂壁纸：库的音频泵逐帧选源，
    // setAudio 随时生效，而重挂会让场景重新下载解析上百 MB 的 scene.pkg
    crate::wallpaper::notify_system_audio(&app, enabled);
    Ok(status(&app))
}

#[tauri::command(rename = "wallpaper_audio_processing_set")]
pub async fn audio_processing_set(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<AudioStatus, String> {
    set_enabled(app, enabled).await
}

#[tauri::command(rename = "wallpaper_audio_processing_status")]
pub fn audio_processing_status(app: tauri::AppHandle) -> AudioStatus {
    status(&app)
}

/// 同步等待捕获进入运行态（壁纸引擎启动排序用：壁纸窗口须等注入服务就绪后再创建）。
/// 失败/超时/未启用返回 false（调用方照常继续，页面内 shim 会经 SSE 重连自愈）。
pub fn wait_until_ready(_app: &tauri::AppHandle, timeout: Duration) -> bool {
    let Some(inner) = inner() else {
        return false;
    };
    let shared = &inner.shared;
    let deadline = Instant::now() + timeout;
    loop {
        match shared.phase() {
            PHASE_RUNNING => return true,
            PHASE_FAILED | PHASE_IDLE => return false,
            _ => {}
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 启动时按设置自启。必须在 wallpaper::init 之前调用：壁纸引擎会等捕获就绪
/// （wait_until_ready 有界等待）再创建壁纸窗口，避免壁纸页先于注入服务加载、
/// 拿到过期的 systemAudio 快照后整段会话无可视化。失败仅记日志。
pub fn start_if_enabled(app: &tauri::AppHandle) {
    let st = status(app);
    if !st.enabled || !st.supported {
        return;
    }
    // 同步标记「启动中」：壁纸引擎的等待依赖相位，不能因 spawn 的任务尚未被
    // 调度而把 STARTING 误判成 IDLE（未启用）而跳过等待
    if let Some(inner) = inner() {
        inner.desired.store(true, Ordering::Relaxed);
        inner.shared.set_phase(PHASE_STARTING);
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = start(app2).await {
            tracing::warn!("audio capture autostart failed: {e}");
        }
    });
}

mod spectrum;

#[cfg(test)]
mod tests {
    /// 真实系统音频的 stop→start 重启链路（bridge 的 stopping 标志必须在 start 时复位，
    /// 否则设置开关/看门狗重启后再也拿不到帧）。默认忽略：需要本机音频权限。
    #[tokio::test]
    #[ignore = "uses real system audio capture"]
    async fn audio_restart_resumes_frames_after_stop() {
        use media_bridge::{audio::AudioConfig, BridgeConfig, MediaBridge};

        let bridge = MediaBridge::new(BridgeConfig {
            audio: AudioConfig {
                fps: 30,
                ..Default::default()
            },
            ..Default::default()
        });
        let wait_frame = |ts: u64| {
            let b = bridge.clone();
            async move {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
                while std::time::Instant::now() < deadline {
                    let f = b.spectrum();
                    if f.ts_ms != ts {
                        return Some(f.ts_ms);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                None
            }
        };
        bridge.ensure_audio().unwrap();
        let t1 = wait_frame(0).await.expect("首次启动后应收到频谱帧");

        bridge.stop_audio();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        bridge.ensure_audio().unwrap();
        let t2 = wait_frame(t1).await.expect("stop 后重启应再次收到频谱帧");
        assert!(t2 > t1);
        bridge.stop_audio();
    }
}
