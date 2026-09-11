//! macOS 实现：ScreenCaptureKit 音频 loopback → FFT → 64 段频谱。
//! 权限：屏幕录制 TCC（CGRequestScreenCaptureAccess 触发系统授权）。
//! 最低 macOS 13（SCStream capturesAudio）。

use std::alloc::{alloc, dealloc, Layout};
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType,
};

use super::{
    AudioShared, AUDIO_SHARED, PHASE_FAILED, PHASE_IDLE, PHASE_RUNNING, PHASE_STARTING,
};

/// ScreenCaptureKit 的音频采样率（固定 48kHz 交错 float32）
const SAMPLE_RATE: f32 = 48_000.0;

pub struct CaptureSession {
    stream: Retained<SCStream>,
    #[allow(dead_code)]
    delegate: Retained<AudioDelegate>,
    #[allow(dead_code)]
    queue: dispatch2::DispatchRetained<dispatch2::DispatchQueue>,
}

/// ObjC 对象跨线程载体。SAFETY: T 仅在 session 互斥锁临界区内被使用，
/// 跨线程传递受互斥保护
pub struct SendPtr<T>(T);
unsafe impl<T> Send for SendPtr<T> {}

/// 平台会话句柄类型（common 层以 imp::Session 引用）
pub type Session = SendPtr<CaptureSession>;

/// 当前平台支持系统音频捕获
pub const SUPPORTED: bool = true;

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
    let Some(shared) = AUDIO_SHARED.get() else {
        return;
    };
    if !shared.is_running() {
        return;
    }
    shared.ever_received.store(true, Ordering::Relaxed);
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
        let Ok(layout) = Layout::from_size_align(needed, 16) else {
            return;
        };
        let storage = alloc(layout);
        if storage.is_null() {
            return;
        }
        let mut list = AudioBufferList {
            number_buffers: 0,
            buffers: [
                AudioBuffer {
                    number_channels: 0,
                    data_byte_size: 0,
                    data: null_mut(),
                },
                AudioBuffer {
                    number_channels: 0,
                    data_byte_size: 0,
                    data: null_mut(),
                },
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
        static FIRST_STATUS: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(true);
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
                super::spectrum::publish_samples(shared, &mono[..mono_len], SAMPLE_RATE);
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

pub fn screen_recording_granted() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}
pub fn start_blocking(
    shared: &Arc<AudioShared>,
    session_slot: &Arc<Mutex<Option<SendPtr<CaptureSession>>>>,
) -> Result<(), String> {
    let mut session = session_slot.lock().map_err(|e| e.to_string())?;
    if session.is_some() {
        shared.set_phase(PHASE_RUNNING); // 已在运行（幂等启动）
        return Ok(());
    }
    shared.set_phase(PHASE_STARTING);
    // tokio 阻塞线程没有 ObjC autorelease pool：所有 ObjC 调用须包在 pool 内
    let result = objc2::rc::autoreleasepool(|_| unsafe { start_inner(shared, &mut session) });
    if result.is_err() {
        shared.set_phase(PHASE_FAILED);
    }
    result
}

unsafe fn start_inner(
    shared: &Arc<AudioShared>,
    session: &mut Option<SendPtr<CaptureSession>>,
) -> Result<(), String> {
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
        .map_err(|_| "启动音频捕获超时".to_string())?;

    shared.set_phase(PHASE_RUNNING);
    tracing::info!("system audio capture started (ScreenCaptureKit loopback)");
    *session = Some(SendPtr(CaptureSession {
        stream,
        delegate,
        queue,
    }));
    Ok(())
}
pub fn stop_blocking(
    shared: &Arc<AudioShared>,
    session_slot: &Arc<Mutex<Option<SendPtr<CaptureSession>>>>,
) -> Result<(), String> {
    let mut session = session_slot.lock().map_err(|e| e.to_string())?;
    shared.set_phase(PHASE_IDLE);
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
