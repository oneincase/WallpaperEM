//! 系统全局音频捕获（二期）：ScreenCaptureKit 音频 loopback → FFT → 64 段频谱。
//!
//! 与 WE 桌面端一致，音频可视化数据来自系统输出（排除本进程自身，壁纸自播的
//! 音乐由一期 shim 内的本地分析覆盖，两路在 shim 内取最大值融合）。
//! 频谱写入 AudioShared（最新帧 + 序号），由内容服务器 /audio-stream SSE 端点
//! 以 ~30Hz 推送给壁纸页内的 EventSource。
//!
//! 权限：屏幕录制 TCC（CGRequestScreenCaptureAccess 触发系统授权）。
//! 最低 macOS 13（SCStream capturesAudio）。

use std::alloc::{alloc, dealloc, Layout};
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType,
};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use serde::Serialize;

use tauri::Manager;

use crate::db;

pub const BANDS: usize = 64;
const WINDOW: usize = 2048;
const SAMPLE_RATE: f32 = 48_000.0;
const PUBLISH_INTERVAL: Duration = Duration::from_millis(33);

/// 最新频谱帧（内容服务器 SSE 读取；音频回调线程写入）
pub struct AudioShared {
    pub running: std::sync::atomic::AtomicBool,
    pub seq: AtomicU64,
    pub bands: Mutex<[f32; BANDS]>,
}

impl AudioShared {
    pub(crate) fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            bands: Mutex::new([0.0; BANDS]),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// 读取最新频谱快照（seq 用于 SSE 判断变化）
    pub fn snapshot(&self) -> (u64, [f32; BANDS]) {
        let bands = self.bands.lock().map(|b| *b).unwrap_or([0.0; BANDS]);
        (self.seq.load(Ordering::Relaxed), bands)
    }

    fn publish(&self, bands: [f32; BANDS]) {
        if let Ok(mut b) = self.bands.lock() {
            *b = bands;
        }
        self.seq.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct AudioCaptureState {
    pub shared: Arc<AudioShared>,
    /// 会话句柄（SCStream 等 ObjC 对象非 Send，经 SendPtr 包裹 + 互斥串行化访问）
    session: Arc<Mutex<Option<SendPtr<CaptureSession>>>>,
}

struct CaptureSession {
    stream: Retained<SCStream>,
    #[allow(dead_code)]
    delegate: Retained<AudioDelegate>,
    #[allow(dead_code)]
    queue: dispatch2::DispatchRetained<dispatch2::DispatchQueue>,
}

/// ObjC 对象跨线程载体。SAFETY: T 仅在 session 互斥锁临界区内被使用，
/// 跨线程传递受互斥保护
struct SendPtr<T>(T);
unsafe impl<T> Send for SendPtr<T> {}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStatus {
    /// 设置开关（持久化）
    pub enabled: bool,
    /// 捕获是否运行中
    pub running: bool,
    /// 屏幕录制权限是否已授予
    pub granted: bool,
}

// ---------- 全局（单会话；delegate 回调无 ivars 通道，经全局访问共享帧与处理状态） ----------

static AUDIO_SHARED: OnceLock<Arc<AudioShared>> = OnceLock::new();
static PROCESSOR: Mutex<Option<Processor>> = Mutex::new(None);

struct Processor {
    ring: Vec<f32>,
    filled: usize,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    hann: Vec<f32>,
    last_publish: Instant,
    prev: [f32; BANDS],
}

impl Processor {
    fn new() -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(WINDOW);
        let hann: Vec<f32> = (0..WINDOW)
            .map(|i| {
                0.5 * (1.0
                    - (2.0 * std::f32::consts::PI * i as f32 / WINDOW as f32).cos())
            })
            .collect();
        Self {
            ring: vec![0.0; WINDOW],
            filled: 0,
            fft,
            hann,
            last_publish: Instant::now() - PUBLISH_INTERVAL,
            prev: [0.0; BANDS],
        }
    }
}

// ---------- 音频回调（dispatch queue 上执行） ----------

define_class!(
    #[unsafe(super(NSObject))]
    struct AudioDelegate;

    unsafe impl NSObjectProtocol for AudioDelegate {}

    unsafe impl SCStreamOutput for AudioDelegate {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        #[allow(non_snake_case)]
        unsafe fn stream_didOutputSampleBuffer_ofType(
            &self,
            _stream: &SCStream,
            sample_buffer: &objc2_core_media::CMSampleBuffer,
            of_type: SCStreamOutputType,
        ) {
            if of_type != SCStreamOutputType::Audio {
                return; // 只要音频；2×2 视频（1fps）忽略
            }
            handle_audio_sample(sample_buffer);
        }
    }
);

fn handle_audio_sample(sample_buffer: &objc2_core_media::CMSampleBuffer) {
    static FIRST_CB: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    let Some(shared) = AUDIO_SHARED.get() else { return };
    if !shared.is_running() {
        return;
    }
    if FIRST_CB.swap(false, Ordering::Relaxed) {
        tracing::info!("audio sample callback: first sample received");
    }
    // CMSampleBuffer 是 CF 类型：引用即对象指针，直接按 CFTypeRef 传给 C API
    let sb = sample_buffer as *const _ as *mut c_void;

    // AudioBufferList（CoreAudio ABI），系统输出最多双声道，预留 2 个 buffer
    #[repr(C)]
    struct AudioBuffer {
        number_channels: u32,
        data_byte_size: u32,
        data: *mut c_void,
    }
    #[repr(C)]
    struct AudioBufferList {
        number_buffers: u32,
        buffers: [AudioBuffer; 2],
    }

    unsafe {
        // 1) 查询所需大小
        let mut needed: usize = 0;
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sb,
            &mut needed,
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            0,
            null_mut(),
        );
        if needed == 0 || needed > 1 << 20 {
            return;
        }
        // 16 字节对齐缓冲
        let Ok(layout) = Layout::from_size_align(needed, 16) else { return };
        let storage = alloc(layout);
        if storage.is_null() {
            return;
        }
        let mut list = AudioBufferList {
            number_buffers: 0,
            buffers: [
                AudioBuffer { number_channels: 0, data_byte_size: 0, data: null_mut() },
                AudioBuffer { number_channels: 0, data_byte_size: 0, data: null_mut() },
            ],
        };
        let mut block: *mut c_void = null_mut();
        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sb,
            null_mut(),
            &mut list as *mut AudioBufferList as *mut c_void,
            needed,
            null_mut(),
            null_mut(),
            0,
            &mut block,
        );
        static FIRST_STATUS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
        if FIRST_STATUS.swap(false, Ordering::Relaxed) {
            tracing::info!(
                "audio extraction: status={status} needed={needed} buffers={} frames={}",
                list.number_buffers,
                CMSampleBufferGetNumSamples(sb)
            );
        }
        if status == 0 && list.number_buffers > 0 {
            let frames = CMSampleBufferGetNumSamples(sb).max(0) as usize;
            // 混合为单声道（非交错：逐 buffer 相加；交错：取每帧第一声道）
            let mut mono = [0.0f32; 4096];
            let mut mono_len = 0usize;
            let n_buf = (list.number_buffers as usize).min(2);
            for b in &list.buffers[..n_buf] {
                let count = (b.data_byte_size as usize) / 4;
                if count == 0 || b.data.is_null() {
                    continue;
                }
                let samples = std::slice::from_raw_parts(b.data as *const f32, count);
                if frames > 0 && count == frames {
                    // 非交错：一个 buffer 一个声道
                    let n = count.min(mono.len());
                    for i in 0..n {
                        mono[i] += samples[i];
                    }
                    mono_len = mono_len.max(n);
                } else {
                    // 交错：单 buffer 含全部声道，取首声道
                    let ch = (b.number_channels as usize).max(1);
                    let n = (count / ch).min(mono.len());
                    for i in 0..n {
                        mono[i] += samples[i * ch];
                    }
                    mono_len = mono_len.max(n);
                }
            }
            if mono_len > 0 {
                let inv = 1.0 / n_buf as f32;
                for v in &mut mono[..mono_len] {
                    *v *= inv;
                }
                publish_samples(shared, &mono[..mono_len]);
            }
        }
        if !storage.is_null() {
            dealloc(storage, layout);
        }
        if !block.is_null() {
            CFRelease(block);
        }
    }
}

/// 累积采样进环形窗；到发布节拍且有整窗数据时做 FFT → 64 段对数频谱
fn publish_samples(shared: &Arc<AudioShared>, samples: &[f32]) {
    let mut guard = match PROCESSOR.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let p = guard.get_or_insert_with(Processor::new);
    for &s in samples {
        p.ring[p.filled] = s;
        p.filled += 1;
        if p.filled == WINDOW {
            p.filled = 0; // 环形回绕
        }
    }
    if p.last_publish.elapsed() < PUBLISH_INTERVAL {
        return;
    }
    p.last_publish = Instant::now();

    // 加窗 → FFT
    let mut buf: Vec<Complex<f32>> = p
        .ring
        .iter()
        .zip(&p.hann)
        .map(|(s, w)| Complex::new(s * w, 0.0))
        .collect();
    p.fft.process(&mut buf);

    // 对数分箱（30Hz ~ min(16kHz, Nyquist)，与 shim 本地分析同分布）
    let bin_hz = SAMPLE_RATE / WINDOW as f32;
    let bins = WINDOW / 2;
    let fmin = 30.0f32;
    let fmax = 16_000.0f32.min(SAMPLE_RATE / 2.0);
    let mut bands = [0.0f32; BANDS];
    let mut prev_edge = fmin.max(0.0) / bin_hz;
    for i in 0..BANDS {
        let f_hi = fmin * (fmax / fmin).powf((i + 1) as f32 / BANDS as f32);
        let lo = prev_edge.round() as usize;
        let hi = ((f_hi / bin_hz).round() as usize).clamp(lo + 1, bins);
        prev_edge = hi as f32;
        let mut m = 0.0f32;
        for b in &buf[lo.min(bins - 1)..hi.min(bins)] {
            m = m.max(b.norm());
        }
        // 归一：汉宁窗全幅正弦幅度 ≈ N/4，稍留增益余量
        let v = (m / 384.0).clamp(0.0, 1.0);
        // 峰值保持衰减（可视化观感：快攻慢衰）
        bands[i] = v.max(p.prev[i] * 0.72);
    }
    p.prev = bands;
    static LAST_SUMMARY: Mutex<Option<Instant>> = Mutex::new(None);
    if let Ok(mut t) = LAST_SUMMARY.lock() {
        if t.map(|t| t.elapsed() > Duration::from_secs(30)).unwrap_or(true) {
            *t = Some(Instant::now());
            let peak = bands.iter().cloned().fold(0.0f32, f32::max);
            tracing::info!("audio spectrum 30s peak band value: {peak:.4}");
        }
    }
    shared.publish(bands);
}

// ---------- CoreMedia / CoreGraphics C 接口 ----------

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sbuf: *mut c_void,
        buffer_list_size_needed: *mut usize,
        buffer_list_out: *mut c_void,
        buffer_list_size: usize,
        block_buffer_allocator: *mut c_void,
        block_buffer_deallocator: *mut c_void,
        flags: u32,
        block_buffer_out: *mut *mut c_void,
    ) -> i32;
    fn CMSampleBufferGetNumSamples(sbuf: *mut c_void) -> i64;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *mut c_void);
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

// ---------- 生命周期 ----------

pub fn init(app: &tauri::AppHandle) -> Result<(), String> {
    let shared = Arc::new(AudioShared::new());
    let _ = AUDIO_SHARED.set(shared.clone());
    app.manage(AudioCaptureState {
        shared,
        session: Arc::new(Mutex::new(None)),
    });
    Ok(())
}

pub fn screen_recording_granted() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// 提取共享句柄（不跨 await 持有 State guard）
fn handles(
    app: &tauri::AppHandle,
) -> Result<(Arc<AudioShared>, Arc<Mutex<Option<SendPtr<CaptureSession>>>>), String> {
    let s = app
        .try_state::<AudioCaptureState>()
        .ok_or("音频捕获状态未就绪")?;
    Ok((s.shared.clone(), s.session.clone()))
}

/// 开启捕获（幂等）。未授权时触发系统授权提示；用户当场同意则直接工作，
/// 否则报错（授权后重开开关即可，无需重启应用）。
pub async fn start(app: tauri::AppHandle) -> Result<(), String> {
    let (shared, session) = handles(&app)?;
    tauri::async_runtime::spawn_blocking(move || start_blocking(&shared, &session))
        .await
        .map_err(|e| format!("音频捕获线程失败: {e}"))?
}

fn start_blocking(
    shared: &Arc<AudioShared>,
    session_slot: &Arc<Mutex<Option<SendPtr<CaptureSession>>>>,
) -> Result<(), String> {
    let mut session = session_slot.lock().map_err(|e| e.to_string())?;
    if session.is_some() {
        return Ok(());
    }
    // tokio 阻塞线程没有 ObjC autorelease pool：所有 ObjC 调用须包在 pool 内
    objc2::rc::autoreleasepool(|_| unsafe {
        if !CGPreflightScreenCaptureAccess() {
            // 触发系统授权对话框（用户可能需到系统设置手动开启）
            CGRequestScreenCaptureAccess();
        }
        // 拉取可捕获内容（completion handler → channel + 超时等待）
        let (tx, rx) = std::sync::mpsc::channel::<Option<Retained<SCShareableContent>>>();
        let block = block2::RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
            // completion 传入的是 +0 autoreleased 引用：必须主动 retain 持有。
            // 若按 +1 消费（from_raw），block 返回后 pool drain 即释放对象 → use-after-free
            let c = Retained::retain(content);
            if !err.is_null() {
                let e = Retained::retain(err);
                if let Some(e) = e {
                    tracing::warn!("SCShareableContent error: {}", e.localizedDescription());
                }
            }
            let _ = tx.send(c);
        });
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            false, false, &block,
        );
        let content = rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "获取可捕获内容超时".to_string())?
            .ok_or("获取可捕获内容失败（可能未授权屏幕录制）")?;

        let displays = content.displays();
        let display = displays
            .iter()
            .next()
            .ok_or("屏幕录制权限未生效：① 系统设置 → 隐私与安全性 → 屏幕录制 允许 WallpaperEM；② 完全退出（⌘Q）重开应用；③ 仍无效则在列表中移除 WallpaperEM 后重新添加")?;

        let filter = SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &display,
            &NSArray::new(),
        );
        let config = SCStreamConfiguration::new();
        config.setWidth(2);
        config.setHeight(2); // 只要音频：2×2 @默认帧率的开销可忽略
        config.setCapturesAudio(true);
        config.setExcludesCurrentProcessAudio(true); // 排除自身进程：壁纸自播音乐由 shim 本地分析覆盖

        let delegate: Retained<AudioDelegate> = msg_send![AudioDelegate::alloc(), init];
        let stream = SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &config,
            // init 的 delegate 是 SCStreamDelegate（生命周期事件，可选）；
            // 帧输出经 addStreamOutput 注册 SCStreamOutput，此处传 None
            None,
        );
        // 属性为 None 即串行队列（SERIAL 是默认值）
        let queue = dispatch2::DispatchQueue::new("wpem-audio", None);
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*delegate),
                SCStreamOutputType::Audio,
                Some(&queue),
            )
            .map_err(|e| format!("注册音频输出失败: {}", e.localizedDescription()))?;

        // 启动捕获（completion → 等待完成）
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let start_block = block2::RcBlock::new(move |err: *mut NSError| {
            if !err.is_null() {
                let e = Retained::retain(err);
                if let Some(e) = e {
                    tracing::error!("SCStream start error: {}", e.localizedDescription());
                }
            }
            let _ = tx.send(());
        });
        stream.startCaptureWithCompletionHandler(Some(&start_block));
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| "启动音频捕获超时")?;

        shared.running.store(true, Ordering::Relaxed);
        tracing::info!("system audio capture started (ScreenCaptureKit loopback)");
        *session = Some(SendPtr(CaptureSession { stream, delegate, queue }));
        Ok(())
    })
}

pub async fn stop(app: tauri::AppHandle) -> Result<(), String> {
    let (shared, session) = handles(&app)?;
    tauri::async_runtime::spawn_blocking(move || stop_blocking(&shared, &session))
        .await
        .map_err(|e| format!("停止音频线程失败: {e}"))?
}

fn stop_blocking(
    shared: &Arc<AudioShared>,
    session_slot: &Arc<Mutex<Option<SendPtr<CaptureSession>>>>,
) -> Result<(), String> {
    let mut session = session_slot.lock().map_err(|e| e.to_string())?;
    shared.running.store(false, Ordering::Relaxed);
    if let Some(s) = session.take() {
        let s = s.0;
        // 同 start：阻塞线程无 pool，包一层 autorelease
        objc2::rc::autoreleasepool(|_| unsafe {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            let block = block2::RcBlock::new(move |_err: *mut NSError| {
                let _ = tx.send(());
            });
            s.stream.stopCaptureWithCompletionHandler(Some(&block));
            let _ = rx.recv_timeout(Duration::from_secs(5));
            let _ = s.stream.removeStreamOutput_type_error(
                ProtocolObject::from_ref(&*s.delegate),
                SCStreamOutputType::Audio,
            );
            tracing::info!("system audio capture stopped");
            Ok::<(), String>(())
        })
        .ok(); // 停止阶段错误不影响状态翻转（running 已置 false）
    }
    Ok(())
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
        granted: screen_recording_granted(),
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

/// 启动时按设置自启（延迟数秒，避开启动风暴；失败仅记日志）
pub fn start_if_enabled(app: &tauri::AppHandle) {
    let enabled = status(app).enabled;
    if !enabled {
        return;
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        if let Err(e) = start(app2.clone()).await {
            tracing::warn!("audio capture autostart failed: {e}");
        }
    });
}
