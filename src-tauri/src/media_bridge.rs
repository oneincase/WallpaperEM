//! 全平台媒体桥接：基于同级仓库 `media-bridge` crate 的进程内单例。
//!
//! 一个 [`MediaBridge`] 同时承载两套此前自研的能力：
//! - 「正在播放」元数据 / 封面 / 反向控制（旧 `now_playing` 各平台实现）
//! - 系统输出音频采集与 64 段频谱（旧 `audio_capture` 各平台实现）
//!
//! 三平台（macOS MediaRemote + CoreAudio Tap / Windows GSMTC + WASAPI loopback /
//! Linux MPRIS + PulseAudio·PipeWire monitor）由 crate 内部统一，本模块只负责
//! 生命周期：启动轮询、选缓存目录、退出回收。音频采集是懒启动的，由
//! [`crate::audio_capture`] 按设置开关调 `ensure_audio` / `stop_audio`。
//!
//! 依赖方式与 webwallgl 一致：file: 依赖同级自有仓库（见仓库根 memory 中的
//! 「webwallgl 上游同步」约定），media-bridge 发版改动后宿主重新编译即可生效。

use std::sync::Arc;
use std::sync::OnceLock;

use media_bridge::audio::AudioConfig;
use media_bridge::{BridgeConfig, MediaBridge};
use tauri::{AppHandle, Manager};

static BRIDGE: OnceLock<Arc<MediaBridge>> = OnceLock::new();

/// 缓存目录的末级目录名（封面落盘 / 歌词缓存都在它下面）。
const CACHE_SUBDIR: &str = "media-bridge";
/// bridge 频谱泵帧率。SSE 按 30Hz 推帧，泵也给到 30Hz，避免「同帧重复推」
/// （bridge 默认 20fps，会让观感比旧实现钝一点）。
const AUDIO_FPS: u32 = 30;

/// 构造并启动全局媒体桥接（幂等）。必须在 tokio runtime 可获取的上下文调用
/// （setup 钩子里即可）：bridge 内部要 `tokio::spawn` 轮询任务。
pub fn start(app: &AppHandle) -> Arc<MediaBridge> {
    if let Some(b) = BRIDGE.get() {
        return b.clone();
    }
    let cache_dir = resolve_cache_dir(app);
    let config = BridgeConfig {
        provider: media_bridge::platform::Provider::Auto,
        cache_dir: Some(cache_dir),
        audio: AudioConfig {
            fps: AUDIO_FPS,
            ..Default::default()
        },
        ..Default::default()
    };

    // MediaBridge::start 内部直接 tokio::spawn，需要 runtime 上下文；setup 钩子
    // 本身是同步的，且不能假设当前线程不在 runtime 内（block_on 在 runtime
    // worker 上会 panic）。起一条独立 OS 线程进 block_on，再通过 channel 取回。
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tauri::async_runtime::block_on(async move {
            let bridge = MediaBridge::new(config);
            bridge.start();
            let _ = tx.send(bridge);
        });
    });
    let bridge = rx.recv().expect("media-bridge 启动线程失联");
    tracing::info!(
        "media-bridge started (provider {}, cache {:?})",
        bridge.provider_name(),
        bridge.config().cache_dir()
    );

    // 元数据 → 壁纸快照的事件泵挂在 now_playing 门面里（SSE 契约保持不变）
    crate::now_playing::attach(bridge.clone());

    let _ = BRIDGE.set(bridge.clone());
    bridge
}

/// 全局 bridge 句柄；音频门面在引擎就绪前会等待它出现。
pub fn bridge() -> Option<Arc<MediaBridge>> {
    BRIDGE.get().cloned()
}

/// 退出前回收：停元数据轮询与音频采集（销毁 Tap / 杀 perl 与采集子进程）。
pub fn stop() {
    if let Some(b) = BRIDGE.get() {
        b.stop();
    }
}

/// 缓存目录：优先 App 缓存目录下的 `media-bridge/`，取不到用 bridge 默认值
/// （~/Library/Caches/media-bridge 等）。
fn resolve_cache_dir(app: &AppHandle) -> std::path::PathBuf {
    app.path()
        .app_cache_dir()
        .map(|d| d.join(CACHE_SUBDIR))
        .unwrap_or_else(|_| BridgeConfig::default_cache_dir())
}
