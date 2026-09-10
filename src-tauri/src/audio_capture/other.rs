//! 非 macOS 平台的音频捕获桩实现。
//!
//! 系统输出回采（loopback）在 Linux 上应走 PipeWire（pipewire-rs / pw-cat），
//! 属于后续版本的功能项；当前开关可开但启动即报「暂不支持」，
//! 相位停在 FAILED，壁纸侧自动回落到库内置的本地分析/模拟源，其余功能不受影响。
//!
//! 接口与 macos.rs 完全对齐（common 层不做平台分支）。

use std::sync::{Arc, Mutex};

use super::{AudioShared, PHASE_FAILED};

/// 平台会话句柄类型（无会话可持有，占位）
pub type Session = ();

/// 是否支持系统音频捕获（status 上报，前端可据此提示）
pub const SUPPORTED: bool = false;

/// 「屏幕录制权限」是 macOS TCC 概念，其他平台恒 false（不可用时前端显示未授权）
pub fn screen_recording_granted() -> bool {
    false
}

pub fn start_blocking(
    shared: &Arc<AudioShared>,
    _session_slot: &Arc<Mutex<Option<Session>>>,
) -> Result<(), String> {
    shared.set_phase(PHASE_FAILED);
    Err("系统音频捕获暂不支持当前平台（macOS 用 ScreenCaptureKit，Linux 待接入 PipeWire）".into())
}

pub fn stop_blocking(
    shared: &Arc<AudioShared>,
    _session_slot: &Arc<Mutex<Option<Session>>>,
) -> Result<(), String> {
    shared.set_phase(super::PHASE_IDLE);
    Ok(())
}
