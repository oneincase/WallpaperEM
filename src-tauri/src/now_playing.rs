//! 系统「正在播放」（Now Playing）：歌名 / 艺人 / 专辑 / 进度 / 封面。
//!
//! WE 的网页与场景壁纸通过 `wallpaperRegisterMediaPropertiesListener` 等四个回调
//! 接收这些信息（webwallgl 侧对应 `SceneInstance.setMedia(MediaSource)`）。
//! 数据写入 [`MediaShared`]，由内容服务器 `/now-playing` SSE 端点推给壁纸页。
//!
//! 平台数据源（拆在 now_playing/ 子模块，接口一致：start/stop/send_command）：
//! - macOS：MediaRemote 私有框架，经 /usr/bin/perl + vendor/mediaremote-adapter
//!   （macOS 15.4+ 按 bundle id 校验，须借 Apple 签名进程加载；详见 macos.rs）
//! - Linux：MPRIS D-Bus 标准接口（org.mpris.MediaPlayer2.*，轮询式；详见 linux.rs）
//!
//! 拿不到数据时降级为「无媒体」而非报错，壁纸侧照常渲染。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use serde::Serialize;
use tauri::AppHandle;

/// WE 播放态：0=停止 1=播放 2=暂停（与库的 MediaPlaybackState 一致）
const STATE_STOPPED: u8 = 0;
const STATE_PLAYING: u8 = 1;
const STATE_PAUSED: u8 = 2;

/// 下发给壁纸页的媒体快照。字段名对齐 webwallgl 的 MediaSnapshot
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MediaSnapshot {
    /// 有没有正在播放的媒体会话；false 时其余字段无意义
    pub has_media: bool,
    pub state: u8,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    /// 进度与总时长，单位**秒**
    pub position: f64,
    pub duration: f64,
    pub has_thumbnail: bool,
    /// 封面 data URL（`data:image/jpeg;base64,...`）。取色交给渲染器侧的 canvas
    /// 完成 —— 浏览器天然会解 JPEG，在 Rust 里解码要多拖上百个依赖包
    #[serde(skip_serializing_if = "String::is_empty")]
    pub thumbnail: String,
    /// 播放列表内的曲目序号（壁纸用它检测换歌）
    pub track_index: i64,
}

/// 最新媒体快照（内容服务器 SSE 读取；adapter 读取线程写入）
pub struct MediaShared {
    /// 快照序号，SSE 端据此判断「有没有变化」，避免把上百 KB 的封面重复推送
    pub seq: AtomicU64,
    snap: Mutex<MediaSnapshot>,
    /// 上一次 publish 的本地时刻，用于播放中外推进度
    published_at: Mutex<Option<Instant>>,
    /// adapter 是否已成功启动过（false 表示这台机器上这条路走不通）
    available: AtomicBool,
}

impl MediaShared {
    fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            snap: Mutex::new(MediaSnapshot::default()),
            published_at: Mutex::new(None),
            available: AtomicBool::new(false),
        }
    }

    /// 当前快照与序号。播放中的 position 按「距上次 publish 的真实时间」外推
    /// —— adapter 只在系统通知时推送，不外推的话 SSE 心跳会一直重复同一个进度值
    pub fn snapshot(&self) -> (u64, MediaSnapshot) {
        let seq = self.seq.load(Ordering::Relaxed);
        let mut snap = self.snap.lock().map(|g| g.clone()).unwrap_or_default();
        if snap.state == STATE_PLAYING {
            let elapsed = self
                .published_at
                .lock()
                .ok()
                .and_then(|g| *g)
                .map(|t| t.elapsed().as_secs_f64())
                .unwrap_or(0.0);
            snap.position += elapsed;
            if snap.duration > 0.0 {
                snap.position = snap.position.clamp(0.0, snap.duration);
            }
        }
        (seq, snap)
    }

    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }

    /// 写入新快照；与旧值相同则不递增 seq（避免 SSE 侧重复推送封面）
    fn publish(&self, next: MediaSnapshot) {
        let mut guard = match self.snap.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if *guard == next {
            return;
        }
        *guard = next;
        drop(guard);
        if let Ok(mut at) = self.published_at.lock() {
            *at = Some(Instant::now());
        }
        self.seq.fetch_add(1, Ordering::Relaxed);
    }
}

static SHARED: OnceLock<Arc<MediaShared>> = OnceLock::new();

/// 全局共享快照（内容服务器与 Tauri 命令共用）
pub fn shared() -> Arc<MediaShared> {
    SHARED.get_or_init(|| Arc::new(MediaShared::new())).clone()
}

/// 停机标志。App 退出时置位，让订阅线程跳出循环并回收子进程/连接。
static STOPPING: AtomicBool = AtomicBool::new(false);

// ---------- 平台实现 ----------

#[cfg(target_os = "macos")]
#[path = "now_playing/macos.rs"]
mod imp;

#[cfg(target_os = "linux")]
#[path = "now_playing/linux.rs"]
mod imp;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[path = "now_playing/other.rs"]
mod imp;

/// 启动常驻订阅（一次性）。重复调用无副作用（进程内只跑一条流）
pub fn start(app: &AppHandle) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    imp::start(app);
}

/// 请求停止媒体订阅并回收资源。App 退出前调用。
pub fn stop() {
    imp::stop();
}

/// MRCommand ID（adapter 的 `send N`）
#[derive(Debug, Clone, Copy)]
pub enum MediaCommand {
    Play,
    Pause,
    TogglePlayPause,
    NextTrack,
    PreviousTrack,
}

impl MediaCommand {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "play" => Self::Play,
            "pause" => Self::Pause,
            "playPause" | "togglePlayPause" => Self::TogglePlayPause,
            "next" | "skipNext" => Self::NextTrack,
            "previous" | "skipPrevious" => Self::PreviousTrack,
            _ => return None,
        })
    }
}

/// 反向控制：壁纸里的播放/暂停、上下一曲按钮转发给真实播放器。
pub fn send_command(cmd: MediaCommand) -> Result<(), String> {
    imp::send_command(cmd)
}
