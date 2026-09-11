//! Windows 实现：WASAPI loopback（系统输出回采）→ FFT → 64 段频谱。
//!
//! 与 WE 桌面端一致，音频可视化数据来自系统输出（排除本进程自身，壁纸自播的
//! 音乐由一期 shim 内的本地分析覆盖，两路在 shim 内取最大值融合）。Windows 的
//! 回采走 WASAPI 共享模式 + `AUDCLNT_STREAMFLAGS_LOOPBACK`：直接抓默认渲染端点
//! 的混音，**不需要任何权限**（与 macOS 的屏幕录制 TCC 形成对比），也听不到
//! 自己的声音被二次采集 —— loopback 抓的是端点混音，不是麦克风。
//!
//! 采样率取混音格式（常见 48k/44.1k，32 位 float 交错），实际值随设备不同而
//! 变化，所以传给 spectrum 而不写死。设备被拔掉/切换默认设备时 `pump` 会失败，
//! 线程自动重开一次会话（2s 退避），不把「换个耳机壁纸就没可视化」留给用户。

use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::core::GUID;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use super::{AudioShared, PHASE_FAILED, PHASE_IDLE, PHASE_RUNNING};

/// loopback 缓冲区时长（100ns 单位 = 1s）。共享模式 loopback 的惯例取值：
/// 太短会在系统负载下丢包，太长只影响首帧延迟。
const BUFFER_DURATION_HNS: i64 = 10_000_000;
/// 无数据时的轮询间隔（设备空闲时 `GetNextPacketSize` 恒为 0）
const IDLE_SLEEP: Duration = Duration::from_millis(5);
/// 会话失败后的重开退避
const REOPEN_BACKOFF: Duration = Duration::from_secs(2);

/// 平台会话句柄：捕获线程的停止标志 + 句柄（stop 时置位并 join）
pub struct CaptureSession {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// 平台会话句柄类型（common 层以 imp::Session 引用）
pub type Session = CaptureSession;

/// 当前平台支持系统音频捕获
pub const SUPPORTED: bool = true;

/// WASAPI loopback 不需要「屏幕录制」类授权：恒 true（status 上报用，
/// 前端据此显示「已就绪」而不是「未授权」）
pub fn screen_recording_granted() -> bool {
    true
}

pub fn start_blocking(
    shared: &Arc<AudioShared>,
    session_slot: &Arc<Mutex<Option<Session>>>,
) -> Result<(), String> {
    let mut slot = session_slot.lock().map_err(|e| e.to_string())?;
    if slot.is_some() {
        return Ok(()); // 幂等
    }
    shared.set_phase(super::PHASE_STARTING);

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let shared2 = shared.clone();
    let stop2 = stop.clone();
    let handle = std::thread::Builder::new()
        .name("wpem-wasapi".into())
        .spawn(move || capture_thread(&shared2, &stop2, tx))
        .map_err(|e| format!("启动音频捕获线程失败: {e}"))?;

    // 等首次会话建立（或明确失败），避免上游把「还在开设备」当成 running
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
            return Err(e);
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
            return Err("启动音频捕获超时（未能在 10 秒内打开默认输出设备）".into());
        }
    }

    *slot = Some(CaptureSession {
        stop,
        handle: Some(handle),
    });
    Ok(())
}

pub fn stop_blocking(
    shared: &Arc<AudioShared>,
    session_slot: &Arc<Mutex<Option<Session>>>,
) -> Result<(), String> {
    let mut slot = session_slot.lock().map_err(|e| e.to_string())?;
    shared.set_phase(PHASE_IDLE);
    if let Some(s) = slot.take() {
        s.stop.store(true, Ordering::SeqCst);
        if let Some(h) = s.handle {
            if h.join().is_err() {
                tracing::warn!("wasapi capture thread panicked");
            }
        }
        tracing::info!("system audio capture stopped");
    }
    Ok(())
}

/// 捕获线程主体：MTA 初始化 → 循环「开会话 → 抽帧」。失败退避重开。
fn capture_thread(
    shared: &Arc<AudioShared>,
    stop: &AtomicBool,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    // WASAPI 是 COM：线程必须先初始化 apartment（MTA 不需要消息泵）
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut announced = false;
    while !stop.load(Ordering::Relaxed) {
        match open_stream() {
            Ok(mut stream) => {
                if !announced {
                    announced = true;
                    shared.set_phase(PHASE_RUNNING);
                    tracing::info!(
                        "system audio capture started (WASAPI loopback, {}Hz x{}ch)",
                        stream.sample_rate,
                        stream.channels
                    );
                    let _ = ready.send(Ok(()));
                }
                if let Err(e) = pump(&mut stream, shared, stop) {
                    tracing::warn!("wasapi capture interrupted: {e}");
                }
            }
            Err(e) => {
                if !announced {
                    shared.set_phase(PHASE_FAILED);
                    tracing::warn!("wasapi open failed: {e}");
                    let _ = ready.send(Err(e));
                    return;
                }
                tracing::warn!("wasapi reopen failed: {e}");
            }
        }
        sleep_interruptible(stop, REOPEN_BACKOFF);
    }
    shared.set_phase(PHASE_IDLE);
}

fn sleep_interruptible(stop: &AtomicBool, total: Duration) {
    let step = Duration::from_millis(100);
    let mut left = total;
    while !left.is_zero() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let d = step.min(left);
        std::thread::sleep(d);
        left -= d;
    }
}

struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    sample_rate: f32,
    channels: usize,
    /// 混音格式是否为 32 位 float（WASAPI 共享模式事实上总是；PCM16 时走整数换算）
    is_float: bool,
}

/// 打开默认渲染端点的 loopback 会话。失败返回面向用户的错误文案。
fn open_stream() -> Result<Stream, String> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| format!("音频设备枚举器不可用: {e}"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| format!("没有可用的默认输出设备: {e}"))?;
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("无法激活输出设备音频客户端: {e}"))?;

        // 共享模式的混音格式（多为 32 位 float 交错/多声道）
        let fmt_ptr: *mut WAVEFORMATEX = client
            .GetMixFormat()
            .map_err(|e| format!("读取混音格式失败: {e}"))?;
        if fmt_ptr.is_null() {
            return Err("混音格式为空".into());
        }
        // WAVEFORMATEX 与其 extensible 变体共用前缀；SubFormat 只在 extensible 下有效
        let (channels, sample_rate, bits, tag, sub_format) = {
            let fmt = &*fmt_ptr;
            let sub = if fmt.wFormatTag as u32 == WAVE_FORMAT_EXTENSIBLE {
                let ext = &*(fmt_ptr as *const WAVEFORMATEXTENSIBLE);
                // WAVEFORMATEXTENSIBLE 是 packed(1)：只能按值取出，不能取引用
                Some(std::ptr::read_unaligned(std::ptr::addr_of!(ext.SubFormat)))
            } else {
                None
            };
            (
                fmt.nChannels as usize,
                fmt.nSamplesPerSec as f32,
                fmt.wBitsPerSample as u32,
                fmt.wFormatTag as u32,
                sub,
            )
        };
        let is_float = tag == WAVE_FORMAT_IEEE_FLOAT
            || sub_format
                .map(|g: GUID| g == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT)
                .unwrap_or(false);
        if !is_float || bits != 32 {
            CoTaskMemFree(Some(fmt_ptr as *const _));
            return Err(format!(
                "暂不支持该音频格式（wFormatTag={tag}, {bits}bit）；\
                 请在「声音设置 → 扬声器 → 高级」中改用 32 位浮点混音格式"
            ));
        }

        let init = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            BUFFER_DURATION_HNS,
            0,
            fmt_ptr,
            None,
        );
        // 混音格式缓冲是 CoTaskMem 分配的，Initialize 之后即可释放（已拷入客户端）
        CoTaskMemFree(Some(fmt_ptr as *const _));
        init.map_err(|e| format!("初始化回采通道失败: {e}"))?;

        let capture: IAudioCaptureClient = client
            .GetService()
            .map_err(|e| format!("获取回采客户端失败: {e}"))?;
        client
            .Start()
            .map_err(|e| format!("启动回采失败: {e}"))?;

        Ok(Stream {
            client,
            capture,
            sample_rate,
            channels: channels.max(1),
            is_float,
        })
    }
}

/// 抽帧循环：把交错多声道下混为单声道，按节拍交给 spectrum。
fn pump(stream: &mut Stream, shared: &Arc<AudioShared>, stop: &AtomicBool) -> Result<(), String> {
    let mut mono: Vec<f32> = Vec::with_capacity(16 * 1024);
    while !stop.load(Ordering::Relaxed) {
        let mut packet = unsafe { stream.capture.GetNextPacketSize() }
            .map_err(|e| format!("读取回采包长度失败: {e}"))?;
        if packet == 0 {
            std::thread::sleep(IDLE_SLEEP);
            continue;
        }
        while packet > 0 {
            let mut data: *mut u8 = null_mut();
            let mut frames: u32 = 0;
            let mut flags: u32 = 0;
            unsafe {
                stream
                    .capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(|e| format!("读取回采缓冲失败: {e}"))?;
            }
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null();
            if silent || frames == 0 {
                mono.extend(std::iter::repeat(0.0f32).take(frames as usize));
            } else if stream.is_float {
                let count = frames as usize * stream.channels;
                let samples = unsafe { std::slice::from_raw_parts(data as *const f32, count) };
                downmix_f32(samples, frames as usize, stream.channels, &mut mono);
            }
            unsafe {
                stream
                    .capture
                    .ReleaseBuffer(frames)
                    .map_err(|e| format!("释放回采缓冲失败: {e}"))?;
            }
            packet = unsafe { stream.capture.GetNextPacketSize() }
                .map_err(|e| format!("读取回采包长度失败: {e}"))?;
        }
        shared.ever_received.store(true, Ordering::Relaxed);
        super::spectrum::publish_samples(shared, &mono, stream.sample_rate);
        mono.clear();
    }
    unsafe {
        let _ = stream.client.Stop();
    }
    Ok(())
}

/// 交错 float32 → 单声道（各声道取平均，与 macOS 后端的 `1/n_buf` 归一一致）
fn downmix_f32(samples: &[f32], frames: usize, channels: usize, out: &mut Vec<f32>) {
    let inv = 1.0 / channels as f32;
    out.reserve(frames);
    for f in 0..frames {
        let base = f * channels;
        let mut acc = 0.0f32;
        for c in 0..channels {
            acc += samples[base + c];
        }
        out.push(acc * inv);
    }
}

#[cfg(test)]
mod tests {
    use super::downmix_f32;

    #[test]
    fn downmix_averages_channels() {
        // 两帧立体声：(1.0, 0.0) 与 (-0.5, 0.5)
        let interleaved = [1.0f32, 0.0, -0.5, 0.5];
        let mut out = Vec::new();
        downmix_f32(&interleaved, 2, 2, &mut out);
        assert_eq!(out, vec![0.5, 0.0]);
    }

    #[test]
    fn downmix_mono_is_identity() {
        let interleaved = [0.25f32, -0.25];
        let mut out = Vec::new();
        downmix_f32(&interleaved, 2, 1, &mut out);
        assert_eq!(out, vec![0.25, -0.25]);
    }
}
