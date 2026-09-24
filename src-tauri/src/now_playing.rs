//! 系统「正在播放」（Now Playing）门面：歌名 / 艺人 / 专辑 / 进度 / 封面。
//!
//! WE 的网页与场景壁纸通过 `wallpaperRegisterMediaPropertiesListener` 等五个回调
//! 接收这些信息（webwallgl 侧对应 `SceneInstance.setMedia(MediaSource)`）。
//! 本模块把桥接快照拍平成旧 wire（[`MediaSnapshot`]，秒 + data URL 封面 +
//! state 0/1/2），写入 [`MediaShared`]，由内容服务器 `/now-playing` SSE 端点推给
//! 壁纸页 —— 渲染器与 webwallgl 侧完全感知不到后端换成了 media-bridge。
//!
//! 三平台数据源统一由 media-bridge crate 提供：
//! - macOS：MediaRemote 私有框架（15.4+ 经内嵌 helper + /usr/bin/perl 权限通道）
//! - Windows：GSMTC（WinRT Windows.Media.Control）
//! - Linux：MPRIS over D-Bus
//!
//! 拿不到数据时降级为「无媒体」而非报错，壁纸侧照常渲染。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use media_bridge::service::Event;
use media_bridge::{MediaBridge, PlaybackState, SourceState, TransportCommand};
use serde::Serialize;

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

/// 最新媒体快照（内容服务器 SSE 读取；bridge 事件泵写入）
pub struct MediaShared {
    /// 快照序号，SSE 端据此判断「有没有变化」，避免把上百 KB 的封面重复推送
    pub seq: AtomicU64,
    snap: Mutex<MediaSnapshot>,
    /// 上一次 publish 的本地时刻，用于播放中外推进度
    published_at: Mutex<Option<Instant>>,
    /// 元数据源是否可用（false 时 SSE 直接回 503，让渲染器保持重连）
    available: AtomicBool,
}

impl MediaShared {
    pub(crate) fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            snap: Mutex::new(MediaSnapshot::default()),
            published_at: Mutex::new(None),
            available: AtomicBool::new(false),
        }
    }

    /// 当前快照与序号。播放中的 position 按「距上次 publish 的真实时间」外推
    /// —— bridge 事件只在变化时推送，不外推的话 SSE 心跳会一直重复同一个进度值
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

    pub(crate) fn set_available(&self, v: bool) {
        self.available.store(v, Ordering::Relaxed);
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

/// 全局共享快照（内容服务器与门面事件泵共用）
pub fn shared() -> Arc<MediaShared> {
    SHARED
        .get_or_init(|| Arc::new(MediaShared::new()))
        .clone()
}

// ---------- bridge 事件泵 ----------

static ATTACHED: AtomicBool = AtomicBool::new(false);

/// 挂上 bridge：订阅变化事件（换曲 / 播放态 / seek / 封面 / 歌词）+ 2s 心跳
/// （可用性翻转与播放进度对齐）。幂等，由 [`crate::media_bridge::start`] 调一次。
pub(crate) fn attach(bridge: Arc<MediaBridge>) {
    if ATTACHED.swap(true, Ordering::SeqCst) {
        return;
    }
    republish(&bridge);
    let mut rx = bridge.subscribe();
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Ok(ev) => on_event(&bridge, &ev),
                    // 慢消费者丢帧：事件只是「该刷新了」的信号，下一次心跳会补齐
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                },
                _ = tick.tick() => republish(&bridge),
            }
        }
    });
}

fn on_event(bridge: &Arc<MediaBridge>, ev: &Event) {
    match ev {
        // 这些事件都意味着快照可能变了，直接重取一次外推快照即可
        Event::Track { .. }
        | Event::Playback { .. }
        | Event::Artwork { .. }
        | Event::Lyrics { .. }
        | Event::Status { .. }
        | Event::Error { .. } => republish(bridge),
        // 频谱由 audio_capture 门面消费，这里不碰
        Event::Spectrum { .. } => {}
    }
}

/// 同步可用性 + 取最新快照并发布
fn republish(bridge: &Arc<MediaBridge>) {
    sync_available(bridge);
    let now = bridge.snapshot_at_now();
    shared().publish(to_snapshot(&now));
}

fn sync_available(bridge: &Arc<MediaBridge>) {
    let ok = bridge
        .status()
        .sources
        .iter()
        .find(|s| s.name == "metadata")
        .map(|s| s.state == SourceState::Running)
        .unwrap_or(false);
    shared().set_available(ok);
}

/// bridge 快照 → 壁纸 wire 快照
fn to_snapshot(now: &media_bridge::NowPlaying) -> MediaSnapshot {
    let mut snap = MediaSnapshot {
        has_media: now.has_media,
        ..Default::default()
    };
    if let Some(t) = &now.track {
        snap.title = t.title.clone();
        snap.artist = t.artist.clone();
        snap.album = t.album.clone();
        // albumArtist 缺省时回退 artist（旧 macOS adapter 也是这个行为）
        snap.album_artist = if !t.album_artist.is_empty() {
            t.album_artist.clone()
        } else {
            t.artist.clone()
        };
        snap.duration = t.duration_ms as f64 / 1000.0;
        snap.track_index = t.track_number.unwrap_or(0) as i64;
        if let Some(art) = &t.artwork {
            if let Some(url) = artwork_data_url(art) {
                snap.has_thumbnail = true;
                snap.thumbnail = url;
            }
        }
    }
    let pb = &now.playback;
    snap.state = if !now.has_media {
        STATE_STOPPED
    } else if pb.state == PlaybackState::Playing {
        STATE_PLAYING
    } else {
        // 暂停 / 停止 / 未知：壁纸侧只有「在放 / 没在放」两种态
        STATE_PAUSED
    };
    snap.position = pb.position_ms as f64 / 1000.0;
    if snap.duration > 0.0 {
        snap.position = snap.position.clamp(0.0, snap.duration);
    }
    snap
}

// ---------- 封面（落盘文件 → data URL，按内容指纹缓存） ----------

struct ArtCache {
    /// 对应 Artwork.key
    key: String,
    data_url: String,
}

static ART_CACHE: OnceLock<Mutex<ArtCache>> = OnceLock::new();

fn art_cache() -> &'static Mutex<ArtCache> {
    ART_CACHE.get_or_init(|| {
        Mutex::new(ArtCache {
            key: String::new(),
            data_url: String::new(),
        })
    })
}

/// 读 bridge 落盘的封面文件并转 data URL。bridge 按内容指纹命名、原子写入，
/// 同一张封面只读一次盘、编一次 base64。
fn artwork_data_url(art: &media_bridge::Artwork) -> Option<String> {
    if let Ok(c) = art_cache().lock() {
        if c.key == art.key && !c.data_url.is_empty() {
            return Some(c.data_url.clone());
        }
    }
    let path = art.path.as_ref()?;
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let url = format!("data:{};base64,{}", art.mime, b64);
    if let Ok(mut c) = art_cache().lock() {
        c.key = art.key.clone();
        c.data_url = url.clone();
    }
    Some(url)
}

// ---------- 反向控制 ----------

/// MRCommand 语义（壁纸侧只有五个按钮）
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

impl From<MediaCommand> for TransportCommand {
    fn from(cmd: MediaCommand) -> Self {
        match cmd {
            MediaCommand::Play => TransportCommand::Play,
            MediaCommand::Pause => TransportCommand::Pause,
            MediaCommand::TogglePlayPause => TransportCommand::PlayPause,
            MediaCommand::NextTrack => TransportCommand::Next,
            MediaCommand::PreviousTrack => TransportCommand::Previous,
        }
    }
}

/// 反向控制（async）：壁纸里的播放/暂停、上下一曲按钮转发给真实播放器。
/// bridge 会先查能力位再发，并回读校验；没生效时返回 `reason`。
pub async fn send_command_async(cmd: MediaCommand) -> Result<(), String> {
    let bridge = crate::media_bridge::bridge()
        .ok_or_else(|| "媒体桥接未初始化".to_string())?;
    let report = bridge
        .control(cmd.into())
        .await
        .map_err(|e| format!("媒体命令失败: {e}"))?;
    if report.outcome.applied {
        Ok(())
    } else {
        Err(report
            .outcome
            .reason
            .unwrap_or_else(|| "播放器未接受该媒体控制请求".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_bridge::platform::mock::MockConfig;
    use media_bridge::platform::Provider;
    use media_bridge::{audio::AudioConfig, BridgeConfig, MediaBridge};

    fn mock_bridge() -> Arc<MediaBridge> {
        let dir = std::env::temp_dir().join(format!("wpem-mb-test-{}", std::process::id()));
        MediaBridge::new(BridgeConfig {
            provider: Provider::Mock,
            mock: MockConfig::default(),
            cache_dir: Some(dir),
            audio: AudioConfig {
                enabled: false,
                ..Default::default()
            },
            lyrics_online: false,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn mock_now_playing_maps_to_wire_snapshot() {
        let bridge = mock_bridge();
        bridge.poll_once().await.unwrap();
        let now = bridge.snapshot_at_now();
        assert!(now.has_media);

        let snap = to_snapshot(&now);
        assert!(snap.has_media);
        assert_eq!(snap.state, STATE_PLAYING);
        assert!(!snap.title.is_empty());
        // albumArtist 缺省回退 artist
        assert!(!snap.album_artist.is_empty());
        assert!(snap.duration > 0.0);
        // mock 给的封面应落盘并被转成 PNG data URL
        assert!(snap.has_thumbnail);
        assert!(snap.thumbnail.starts_with("data:image/png;base64,"));

        // 再次转换命中指纹缓存，仍然可用
        let snap2 = to_snapshot(&now);
        assert_eq!(snap.thumbnail, snap2.thumbnail);
    }

    #[tokio::test]
    async fn empty_snapshot_maps_to_stopped_wire() {
        let snap = to_snapshot(&media_bridge::NowPlaying::empty(0));
        assert!(!snap.has_media);
        assert_eq!(snap.state, STATE_STOPPED);
        assert_eq!(snap.position, 0.0);
    }

    #[test]
    fn command_parse_and_mapping() {
        assert!(matches!(
            MediaCommand::parse("playPause"),
            Some(MediaCommand::TogglePlayPause)
        ));
        assert!(matches!(
            MediaCommand::parse("skipNext"),
            Some(MediaCommand::NextTrack)
        ));
        assert!(MediaCommand::parse("seek").is_none());
        assert!(matches!(
            TransportCommand::from(MediaCommand::NextTrack),
            TransportCommand::Next
        ));
    }
}
