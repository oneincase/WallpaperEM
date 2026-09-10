//! 系统全局音频捕获（二期）：系统输出 loopback → FFT → 64 段频谱。
//!
//! 与 WE 桌面端一致，音频可视化数据来自系统输出（排除本进程自身，壁纸自播的
//! 音乐由一期 shim 内的本地分析覆盖，两路在 shim 内取最大值融合）。
//! 频谱写入 AudioShared（最新帧 + 序号），由内容服务器 /audio-stream SSE 端点
//! 以 ~30Hz 推送给壁纸页内的 EventSource。
//!
//! 平台实现拆在 audio_capture/ 子模块：
//! - macOS：ScreenCaptureKit 音频 loopback（macOS 13+，屏幕录制 TCC 授权）
//! - 其他平台：桩实现（启动即报不支持，壁纸回落库内置分析；Linux 待接入 PipeWire）

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::Manager;

use crate::db;

pub const BANDS: usize = 64;
// 以下常量只被 macOS 捕获实现（FFT 窗口/发布节拍）使用
#[cfg(target_os = "macos")]
const WINDOW: usize = 2048;
#[cfg(target_os = "macos")]
const SAMPLE_RATE: f32 = 48_000.0;
#[cfg(target_os = "macos")]
const PUBLISH_INTERVAL: Duration = Duration::from_millis(33);

/// 捕获生命周期相位（AudioShared.phase）
pub const PHASE_IDLE: u8 = 0; // 未启动（开关关闭或已停止）
pub const PHASE_STARTING: u8 = 1; // 启动中（SCShareableContent/SCStream 建立期间）
pub const PHASE_RUNNING: u8 = 2; // 运行中（样本持续到达）
pub const PHASE_FAILED: u8 = 3; // 本次启动失败（权限缺失/超时等）

/// 最新频谱帧（内容服务器 SSE 读取；音频回调线程写入）
pub struct AudioShared {
    /// 捕获生命周期相位（PHASE_*）
    phase: AtomicU8,
    /// 是否收到过至少一个音频样本（看门狗只对「曾正常工作后卡死」的会话重启，
    /// 避免对从未投递样本的环境做无意义的反复重启）
    pub ever_received: AtomicBool,
    pub seq: AtomicU64,
    pub bands: Mutex<[f32; BANDS]>,
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

    /// 写入新频谱帧（仅 macOS 捕获回调使用；桩实现不产生数据）
    #[cfg(target_os = "macos")]
    pub(crate) fn publish(&self, bands: [f32; BANDS]) {
        if let Ok(mut b) = self.bands.lock() {
            *b = bands;
        }
        self.seq.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct AudioCaptureState {
    pub shared: Arc<AudioShared>,
    /// 平台会话句柄（macOS = SCStream 会话；其余平台为占位类型）
    session: Arc<Mutex<Option<imp::Session>>>,
}

// ---------- 平台实现（macOS = ScreenCaptureKit；其余 = 桩，见 audio_capture/other.rs） ----------

#[cfg(target_os = "macos")]
#[path = "audio_capture/macos.rs"]
mod imp;

#[cfg(not(target_os = "macos"))]
#[path = "audio_capture/other.rs"]
mod imp;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStatus {
    /// 设置开关（持久化）
    pub enabled: bool,
    /// 捕获是否运行中
    pub running: bool,
    /// 屏幕录制权限是否已授予（非 macOS 恒 false）
    pub granted: bool,
    /// 当前平台是否支持系统音频捕获（macOS 支持；Linux 待接入 PipeWire）
    pub supported: bool,
}

// ---------- 全局（单会话；delegate 回调无 ivars 通道，经全局访问共享帧与处理状态） ----------

static AUDIO_SHARED: OnceLock<Arc<AudioShared>> = OnceLock::new();

pub fn init(app: &tauri::AppHandle) -> Result<(), String> {
    let shared = Arc::new(AudioShared::new());
    let _ = AUDIO_SHARED.set(shared.clone());
    app.manage(AudioCaptureState {
        shared,
        session: Arc::new(Mutex::new(None)),
    });
    Ok(())
}

/// 提取共享句柄（不跨 await 持有 State guard）
fn handles(
    app: &tauri::AppHandle,
) -> Result<(Arc<AudioShared>, Arc<Mutex<Option<imp::Session>>>), String> {
    let s = app
        .try_state::<AudioCaptureState>()
        .ok_or("音频捕获状态未就绪")?;
    Ok((s.shared.clone(), s.session.clone()))
}

/// 开启捕获（幂等）。未授权时触发系统授权提示；用户当场同意则直接工作，
/// 否则报错（授权后重开开关即可，无需重启应用）。
pub async fn start(app: tauri::AppHandle) -> Result<(), String> {
    let (shared, session) = handles(&app)?;
    tauri::async_runtime::spawn_blocking(move || imp::start_blocking(&shared, &session))
        .await
        .map_err(|e| format!("音频捕获线程失败: {e}"))?
}

pub async fn stop(app: tauri::AppHandle) -> Result<(), String> {
    let (shared, session) = handles(&app)?;
    tauri::async_runtime::spawn_blocking(move || imp::stop_blocking(&shared, &session))
        .await
        .map_err(|e| format!("停止音频线程失败: {e}"))?
}

pub fn status(app: &tauri::AppHandle) -> AudioStatus {
    let enabled = app
        .try_state::<Arc<Mutex<rusqlite::Connection>>>()
        .and_then(|db| {
            let conn = db.lock().ok()?;
            Some(
                db::get_setting(&conn, "wallpaper_audio_processing")
                    .map(|v| v == "true")
                    .unwrap_or(false),
            )
        })
        .unwrap_or(false);
    let running = app
        .try_state::<AudioCaptureState>()
        .map(|s| s.shared.is_running())
        .unwrap_or(false);
    AudioStatus {
        enabled,
        running,
        granted: imp::screen_recording_granted(),
        supported: imp::SUPPORTED,
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
pub fn wait_until_ready(app: &tauri::AppHandle, timeout: Duration) -> bool {
    let Some(state) = app.try_state::<AudioCaptureState>() else {
        return false;
    };
    let shared = state.shared.clone();
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
    let enabled = status(app).enabled;
    if !enabled {
        return;
    }
    // 同步标记「启动中」：壁纸引擎的等待依赖相位，不能因 spawn 的任务尚未被
    // 调度而把 STARTING 误判成 IDLE（未启用）而跳过等待
    if let Some(state) = app.try_state::<AudioCaptureState>() {
        state.shared.set_phase(PHASE_STARTING);
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = start(app2.clone()).await {
            tracing::warn!("audio capture autostart failed: {e}");
        }
    });
}
