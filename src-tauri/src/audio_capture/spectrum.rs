//! 频谱计算（macOS ScreenCaptureKit / Windows WASAPI loopback 共用）。
//!
//! 采样累积进环形窗 → 到发布节拍做加窗 FFT → 64 段对数频谱 → 峰值保持衰减。
//! 两端共用同一份实现是刻意的：壁纸页里的可视化对「同一首歌在 Mac 与 Windows
//! 上看起来一致」有直接观感要求，分箱/归一/衰减任何一处走样都会看出来。
//!
//! 采样率由调用方传入（macOS ScreenCaptureKit 固定 48k；WASAPI 取混音格式，
//! 常见 48k 也可能是 44.1k），除 `bin_hz` 换算外所有逻辑与采样率无关。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use super::{AudioShared, BANDS};

/// FFT 窗长（采样点数）
pub const WINDOW: usize = 2048;
/// 频谱发布节拍（≈30Hz，内容服务器 SSE 的推送节奏）
pub const PUBLISH_INTERVAL: Duration = Duration::from_millis(33);

struct Processor {
    ring: Vec<f32>,
    filled: usize,
    fft: Arc<dyn rustfft::Fft<f32>>,
    hann: Vec<f32>,
    last_publish: Instant,
    prev: [f32; BANDS],
    sample_rate: f32,
}

impl Processor {
    fn new(sample_rate: f32) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(WINDOW);
        let hann: Vec<f32> = (0..WINDOW)
            .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / WINDOW as f32).cos()))
            .collect();
        Self {
            ring: vec![0.0; WINDOW],
            filled: 0,
            fft,
            hann,
            last_publish: Instant::now() - PUBLISH_INTERVAL,
            prev: [0.0; BANDS],
            sample_rate,
        }
    }
}

/// 单会话处理器。采样率变化（换音频设备）时按新采样率重建。
static PROCESSOR: Mutex<Option<Processor>> = Mutex::new(None);

/// 累积采样进环形窗；到发布节拍且有整窗数据时做 FFT → 64 段对数频谱
pub fn publish_samples(shared: &Arc<AudioShared>, samples: &[f32], sample_rate: f32) {
    let mut guard = match PROCESSOR.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if guard
        .as_ref()
        .map(|p| (p.sample_rate - sample_rate).abs() > 0.5)
        .unwrap_or(false)
    {
        *guard = None; // 采样率变了（切了音频设备）：重算 bin 划分
    }
    let p = guard.get_or_insert_with(|| Processor::new(sample_rate));
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
    let bin_hz = p.sample_rate / WINDOW as f32;
    let bins = WINDOW / 2;
    let fmin = 30.0f32;
    let fmax = 16_000.0f32.min(p.sample_rate / 2.0);
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
        if t.map(|t| t.elapsed() > Duration::from_secs(30))
            .unwrap_or(true)
        {
            *t = Some(Instant::now());
            let peak = bands.iter().cloned().fold(0.0f32, f32::max);
            tracing::info!("audio spectrum 30s peak band value: {peak:.4}");
        }
    }
    shared.publish(bands);
}
