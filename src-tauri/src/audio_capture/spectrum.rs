//! 频谱计算（macOS ScreenCaptureKit / Windows WASAPI loopback 共用）。
//!
//! 采样累积进环形窗 → 到发布节拍做加窗 FFT → 64 段对数频谱 → 动态增益（AGC）
//! → 峰值保持衰减。两端共用同一份实现是刻意的：壁纸页里的可视化对「同一首歌
//! 在 Mac 与 Windows 上看起来一致」有直接观感要求，分箱/归一/增益/衰减任何一处
//! 走样都会看出来。
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

// ---------- 动态增益（AGC）----------
//
// 固定归一（m/384）按「满幅正弦占满单个 FFT bin」标定；真实音乐经系统混音 +
// 汉宁窗后各频段幅度通常只有 0.02~0.2，壁纸拿到的频谱整体偏小、波动不明显。
// AGC 以帧峰为参考自动提/落增益，把典型音乐的频谱拉到接近满幅，同时：
// - 不动频谱形状：全频段乘同一个增益，高低频相对关系保持；
// - 不放大底噪：峰值低于噪底门限时按静音处理，增益快速回落；
// - 快攻慢放：音乐变响增益快速压下（防过冲），变轻缓慢抬升（防呼吸感抽动）。
/// 帧峰增益后的目标值（0.7 + 0.3 余量 = 1.0 满幅：瞬时峰值有头部空间，不普遍削顶。
/// 注意必须 ≤ 1.0 —— 频谱值在 1.0 处钳制，目标超过它等于故意制造削顶）
const AGC_TARGET: f32 = 0.7;
/// 增益上限：静音段的残余底噪 ×50 仍不足 0.02，视觉上不可见
const AGC_GAIN_MAX: f32 = 100.0;
/// 低于此帧峰视为静音/底噪，停止正常 AGC 并回落增益
const AGC_NOISE_FLOOR: f32 = 0.005;
/// 帧峰跟随器的每帧衰减（30Hz 下 ≈1.1s 落回一半）：决定增益抬升的参考速度
const AGC_PEAK_DECAY: f32 = 0.985;
/// 增益下调速度（帧峰变大时，~100ms 收敛）
const AGC_ATTACK: f32 = 0.3;
/// 增益上调速度（帧峰变小时，~2s 内抬到位，肉眼无抽动）
const AGC_RELEASE: f32 = 0.02;
/// 静音期间增益的回落速度（从 10× 落回 1× 约 2s，之后底噪不再被放大）
const AGC_SILENCE_DECAY: f32 = 0.97;

struct Processor {
    ring: Vec<f32>,
    filled: usize,
    fft: Arc<dyn rustfft::Fft<f32>>,
    hann: Vec<f32>,
    last_publish: Instant,
    prev: [f32; BANDS],
    sample_rate: f32,
    /// AGC：帧峰跟随器（静音时不更新，靠衰减自然回落）
    peak: f32,
    /// AGC：当前生效增益（1.0 = 不放大）
    gain: f32,
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
            peak: 0.0,
            gain: 1.0,
        }
    }

    /// 单帧 AGC：入参为未增益的帧峰，返回本帧应乘的增益。
    /// 增益是状态量，静音帧不更新峰值、只让增益向 1 回落。
    fn agc_gain(&mut self, frame_peak: f32) -> f32 {
        if frame_peak < AGC_NOISE_FLOOR {
            self.gain = (self.gain * AGC_SILENCE_DECAY).max(1.0);
            return self.gain;
        }
        self.peak = frame_peak.max(self.peak * AGC_PEAK_DECAY);
        let desired = (AGC_TARGET / self.peak).clamp(1.0, AGC_GAIN_MAX);
        let k = if desired < self.gain {
            AGC_ATTACK
        } else {
            AGC_RELEASE
        };
        self.gain += (desired - self.gain) * k;
        self.gain
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
        let mut frame_peak = 0.0f32;
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
            // 固定归一：汉宁窗全幅正弦幅度 ≈ N/4，稍留增益余量（动态倍数在下面补足）
            let v = (m / 384.0).clamp(0.0, 1.0);
            if v > frame_peak {
                frame_peak = v;
            }
            bands[i] = v;
        }
        // 动态倍数：把真实音乐普遍偏小的频谱拉到接近满幅（先于峰值保持，衰减曲线同频缩放）
        let gain = p.agc_gain(frame_peak);
        if gain > 1.0 {
            for b in &mut bands {
                *b = (*b * gain).min(1.0);
            }
        }
        // 峰值保持衰减（可视化观感：快攻慢衰）
        for i in 0..BANDS {
            bands[i] = bands[i].max(p.prev[i] * 0.72);
        }
        p.prev = bands;
        static LAST_SUMMARY: Mutex<Option<Instant>> = Mutex::new(None);
        if let Ok(mut t) = LAST_SUMMARY.lock() {
            if t.map(|t| t.elapsed() > Duration::from_secs(30))
                .unwrap_or(true)
            {
                *t = Some(Instant::now());
                let peak = bands.iter().cloned().fold(0.0f32, f32::max);
                tracing::info!("audio spectrum 30s: peak band {peak:.3} @ gain {gain:.2}");
            }
        }
    shared.publish(bands);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn processor() -> Processor {
        Processor::new(48_000.0)
    }

    /// 静音音乐（帧峰 ~0.05）应被拉到目标附近；响亮音乐（≥目标）不放大
    #[test]
    fn agc_pulls_quiet_music_toward_target_and_spares_loud() {
        let mut p = processor();
        let mut g = 1.0;
        for _ in 0..300 {
            g = p.agc_gain(0.05);
        }
        assert!(g > 5.0, "quiet music gain should rise, got {g}");
        // 增益收敛到 TARGET/0.05 ≈ 14（远未到上限），放大后的帧峰应接近目标值
        assert!((g - AGC_TARGET / 0.05).abs() < 0.1, "gain should settle at TARGET/peak, got {g}");
        assert!((0.05 * g) > 0.49, "amplified peak should approach the target");

        let mut p2 = processor();
        for _ in 0..60 {
            p2.agc_gain(0.9);
        }
        assert!(
            (p2.gain - 1.0).abs() < 1e-4,
            "loud music must not be attenuated/boosted, gain={}",
            p2.gain
        );
    }

    /// 底噪帧峰（<噪底）不应把增益抬上去；且长时间静音后增益回落到 1
    #[test]
    fn agc_ignores_noise_floor_and_decays_in_silence() {
        let mut p = processor();
        for _ in 0..120 {
            p.agc_gain(0.08); // 先让增益升起来
        }
        assert!(p.gain > 1.0);
        for _ in 0..600 {
            p.agc_gain(0.001); // 底噪
        }
        assert!(
            (p.gain - 1.0).abs() < 1e-4,
            "gain must fall back to 1x in silence, got {}",
            p.gain
        );
        // 静音期间峰值跟随器刻意保持不更新（音乐恢复时增益能立刻给出参考值）
        assert!((p.peak - 0.08).abs() < 1e-4, "peak must not drift in silence");
    }

    /// 增益对帧峰的响应：突然变响时快速压下，不会持续削顶放大
    #[test]
    fn agc_attacks_fast_on_loud_transients() {
        let mut p = processor();
        for _ in 0..120 {
            p.agc_gain(0.05);
        }
        let hot = p.gain;
        for _ in 0..5 {
            p.agc_gain(0.9);
        }
        assert!(
            p.gain < hot * 0.5,
            "gain should collapse quickly on loud input: {hot} -> {}",
            p.gain
        );
    }
}
