//! 频谱后处理：media-bridge 的 64 段 u8（0..255，-70dB..0dB 对数映射）
//! → 壁纸契约的 64 段 f32（0..1），再做动态增益（AGC）与峰值保持。
//!
//! FFT / 分箱 / 降采样由 media-bridge 在三平台统一完成；这里保留 WallpaperEM
//! 旧实现的「观感层」：真实音乐经系统混音 + 汉宁窗后各频段普遍偏小，AGC 把
//! 典型音乐拉到接近满幅，峰值保持让下落动画平滑。节拍按 30Hz 标定
//! （监控线程 33ms 一帧）。

use super::BANDS;

/// 帧峰增益后的目标值（≤1.0：频谱在 1.0 处钳制，超过等于故意削顶）
const AGC_TARGET: f32 = 0.7;
/// 增益上限：静音段残余底噪放大后仍不可见
const AGC_GAIN_MAX: f32 = 100.0;
/// 低于此帧峰视为静音/底噪，停止正常 AGC 并回落增益
const AGC_NOISE_FLOOR: f32 = 0.005;
/// 帧峰跟随器的每帧衰减（30Hz 下 ≈1.1s 落回一半）
const AGC_PEAK_DECAY: f32 = 0.985;
/// 增益下调速度（帧峰变大时，~100ms 收敛）
const AGC_ATTACK: f32 = 0.3;
/// 增益上调速度（帧峰变小时，~2s 抬到位，避免呼吸感抽动）
const AGC_RELEASE: f32 = 0.02;
/// 静音期间增益回落速度（从 10× 落回 1× 约 2s）
const AGC_SILENCE_DECAY: f32 = 0.97;
/// 峰值保持衰减系数（快攻慢衰的下落曲线）
const PEAK_HOLD_DECAY: f32 = 0.72;

pub struct BandsPost {
    /// AGC：帧峰跟随器（静音时不更新，靠衰减自然回落）
    peak: f32,
    /// AGC：当前生效增益（1.0 = 不放大）
    gain: f32,
    /// 上一帧输出（峰值保持用）
    prev: [f32; BANDS],
}

impl BandsPost {
    pub fn new() -> Self {
        Self {
            peak: 0.0,
            gain: 1.0,
            prev: [0.0; BANDS],
        }
    }

    /// 停用时复位，避免上个会话的增益/残影带进下次采集
    pub fn reset(&mut self) {
        self.peak = 0.0;
        self.gain = 1.0;
        self.prev = [0.0; BANDS];
    }

    /// 处理一帧：u8 0..255 → f32 0..1 → AGC → 峰值保持。
    pub fn process(&mut self, raw: &[u8], out: &mut [f32; BANDS]) {
        for (i, &v) in raw.iter().take(BANDS).enumerate() {
            out[i] = v as f32 / 255.0;
        }
        let frame_peak = out.iter().copied().fold(0.0f32, f32::max);
        let gain = self.agc_gain(frame_peak);
        if gain > 1.0 {
            for b in out.iter_mut() {
                *b = (*b * gain).min(1.0);
            }
        }
        for (cur, prev) in out.iter_mut().zip(self.prev.iter()) {
            *cur = (*cur).max(*prev * PEAK_HOLD_DECAY);
        }
        self.prev = *out;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(level: u8) -> Vec<u8> {
        vec![level; BANDS]
    }

    /// 静音音乐（u8 ≈ 13，约 0.05）应被增益拉起；响亮音乐不放大
    #[test]
    fn agc_pulls_quiet_music_toward_target_and_spares_loud() {
        let mut p = BandsPost::new();
        let mut out = [0.0; BANDS];
        for _ in 0..300 {
            p.process(&frame(13), &mut out);
        }
        assert!(out[0] > 0.49, "quiet band should approach target, got {}", out[0]);

        let mut p2 = BandsPost::new();
        for _ in 0..60 {
            p2.process(&frame(230), &mut out);
        }
        assert!(
            (p2.gain - 1.0).abs() < 1e-4,
            "loud music must not be boosted, gain={}",
            p2.gain
        );
    }

    /// 长时间静音后增益回落到 1，且输出衰减到接近 0
    #[test]
    fn gain_decays_and_silence_fades_out() {
        let mut p = BandsPost::new();
        let mut out = [0.0; BANDS];
        for _ in 0..120 {
            p.process(&frame(20), &mut out);
        }
        assert!(p.gain > 1.0);
        for _ in 0..600 {
            p.process(&frame(0), &mut out);
        }
        assert!((p.gain - 1.0).abs() < 1e-4, "gain={}", p.gain);
        assert!(out[0] < 0.01, "silence should fade out, got {}", out[0]);
    }

    /// 突然变响时增益快速压下，不会持续削顶放大
    #[test]
    fn agc_attacks_fast_on_loud_transients() {
        let mut p = BandsPost::new();
        let mut out = [0.0; BANDS];
        for _ in 0..120 {
            p.process(&frame(13), &mut out);
        }
        let hot = p.gain;
        for _ in 0..5 {
            p.process(&frame(230), &mut out);
        }
        assert!(p.gain < hot * 0.5, "gain {hot} -> {}", p.gain);
    }

    #[test]
    fn reset_clears_state() {
        let mut p = BandsPost::new();
        let mut out = [0.0; BANDS];
        for _ in 0..120 {
            p.process(&frame(20), &mut out);
        }
        p.reset();
        assert!((p.gain - 1.0).abs() < 1e-4);
        assert_eq!(p.prev, [0.0; BANDS]);
    }
}
