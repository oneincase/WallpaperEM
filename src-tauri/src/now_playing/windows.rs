//! Windows「正在播放」：GSMTC（Global System Media Transport Controls）轮询实现。
//!
//! GSMTC 是 Windows 10 1709+ 的系统级媒体集成接口（`Windows.Media.Control`），
//! 覆盖 Spotify、浏览器、各类 UWP/Win32 播放器 —— 对应 macOS 的 MediaRemote 与
//! Linux 的 MPRIS，且是公开稳定 API，不需要任何绕行（与 Linux 的 MPRIS 后端同构）。
//!
//! 实现选择**轮询**（1s 间隔）而不是 `MediaPropertiesChanged` 事件订阅：
//! 事件要求桌面应用自建事件循环/RoGetActivationFactory 的注册表条目，而快照本
//! 来就带进度外推（MediaShared.snapshot 按 published_at 推进），1s 粒度的状态/
//! 换歌刷新对壁纸场景足够 —— 与 linux.rs 的取舍一致，两端行为可预期地相同。
//!
//! 封面：`TryGetMediaPropertiesAsync` 拿到的 `Thumbnail` 是 IRandomAccessStream，
//! 读成字节后按魔数判定 MIME 编成 data URL（与 macOS/Linux 后端的输出形态一致）。
//! 按「标题+艺人+专辑」缓存，只在换曲时重新读取，避免每秒解一次流。

use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::AppHandle;
use windows::core::Interface;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::{DataReader, IInputStream, IRandomAccessStreamReference};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use super::{
    MediaCommand, MediaShared, MediaSnapshot, STATE_PAUSED, STATE_PLAYING, STATE_STOPPED, STOPPING,
};

/// 状态轮询间隔
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 拿不到会话管理器时的重试间隔（媒体集成非关键路径，不忙等）
const RETRY_INTERVAL: Duration = Duration::from_secs(10);
/// 封面体积上限（防异常播放器塞来超大图）
const MAX_ARTWORK_BYTES: u64 = 4 * 1024 * 1024;
/// WinRT TimeSpan 的计时单位：100 纳秒
const TICKS_PER_SEC: f64 = 10_000_000.0;

/// 启动常驻轮询线程（一次性保证由公共层 now_playing::start 负责）
pub fn start(_app: &AppHandle) {
    let shared = super::shared();
    std::thread::Builder::new()
        .name("now-playing-gsmtc".into())
        .spawn(move || poll_loop(&shared))
        .ok();
}

/// 请求停止：轮询线程下一轮检查退出
pub fn stop() {
    STOPPING.store(true, Ordering::SeqCst);
}

/// 反向控制：重新定位当前会话并调用对应 GSMTC Try*Async。
pub fn send_command(cmd: MediaCommand) -> Result<(), String> {
    init_apartment();
    let mgr = manager()?;
    let session = mgr
        .GetCurrentSession()
        .map_err(|_| "没有正在播放的媒体会话".to_string())?;
    let ok: bool = match cmd {
        MediaCommand::Play => session.TryPlayAsync().map_err(err_str)?.get(),
        MediaCommand::Pause => session.TryPauseAsync().map_err(err_str)?.get(),
        MediaCommand::TogglePlayPause => {
            session.TryTogglePlayPauseAsync().map_err(err_str)?.get()
        }
        MediaCommand::NextTrack => session.TrySkipNextAsync().map_err(err_str)?.get(),
        MediaCommand::PreviousTrack => session.TrySkipPreviousAsync().map_err(err_str)?.get(),
    }
    .map_err(err_str)?;
    if ok {
        Ok(())
    } else {
        Err("系统拒绝了该媒体控制请求".into())
    }
}

fn err_str(e: windows::core::Error) -> String {
    e.to_string()
}

/// 轮询线程入口：MTA 初始化一次，之后复用同一个会话管理器
fn poll_loop(shared: &std::sync::Arc<MediaShared>) {
    init_apartment();
    let mut mgr: Option<SessionManager> = None;
    let mut cache = ArtworkCache::default();
    while !STOPPING.load(Ordering::Relaxed) {
        if mgr.is_none() {
            match manager() {
                Ok(m) => {
                    tracing::info!("now playing: GSMTC session manager ready");
                    mgr = Some(m);
                }
                Err(e) => {
                    tracing::debug!("now playing: GSMTC unavailable: {e}");
                    sleep_interruptible(RETRY_INTERVAL);
                    continue;
                }
            }
        }
        match query(mgr.as_ref().unwrap(), &mut cache) {
            Ok(snap) => shared.publish(snap),
            Err(e) => tracing::debug!("now playing: query failed: {e}"),
        }
        sleep_interruptible(POLL_INTERVAL);
    }
}

/// 分段睡眠：stop() 后最多 100ms 就退出，不被 1s/10s 间隔拖住
fn sleep_interruptible(total: Duration) {
    let step = Duration::from_millis(100);
    let mut left = total;
    while !left.is_zero() {
        if STOPPING.load(Ordering::Relaxed) {
            return;
        }
        let d = step.min(left);
        std::thread::sleep(d);
        left -= d;
    }
}

fn init_apartment() {
    // 0x80010106 (RPC_E_CHANGED_MODE) = 已是别的 apartment，无需再初始化
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn manager() -> Result<SessionManager, String> {
    let op = SessionManager::RequestAsync().map_err(err_str)?;
    op.get().map_err(err_str)
}

/// 读取当前会话 → 快照。无会话时返回「无媒体」（不是错误）
fn query(mgr: &SessionManager, cache: &mut ArtworkCache) -> Result<MediaSnapshot, String> {
    let session = match mgr.GetCurrentSession() {
        Ok(s) => s,
        Err(_) => return Ok(MediaSnapshot::default()),
    };
    let props = session
        .TryGetMediaPropertiesAsync()
        .map_err(err_str)?
        .get()
        .map_err(err_str)?;

    let title = hstr(props.Title());
    let artist = hstr(props.Artist());
    let album = hstr(props.AlbumTitle());
    let album_artist = hstr(props.AlbumArtist());
    let track_index = props.TrackNumber().unwrap_or(0) as i64;

    let state = match session
        .GetPlaybackInfo()
        .and_then(|info| info.PlaybackStatus())
    {
        Ok(s) if s == PlaybackStatus::Playing => STATE_PLAYING,
        Ok(s) if s == PlaybackStatus::Paused => STATE_PAUSED,
        // Stopped/Closed/Changing/Opened 一律按停止处理（壁纸侧只区分这三态）
        Ok(_) => STATE_STOPPED,
        Err(_) => STATE_STOPPED,
    };

    let (position, duration) = timeline(&session);

    let (thumbnail, has_thumbnail) = match cache.get(&title, &artist, &album, &album_artist) {
        Some(url) => (url, true),
        None => (String::new(), false),
    };

    Ok(MediaSnapshot {
        has_media: true,
        state,
        title,
        artist,
        album,
        album_artist,
        position,
        duration,
        has_thumbnail,
        thumbnail,
        track_index,
    })
}

fn timeline(session: &Session) -> (f64, f64) {
    let Ok(tl) = session.GetTimelineProperties() else {
        return (0.0, 0.0);
    };
    let pos = tl
        .Position()
        .map(|t| t.Duration as f64 / TICKS_PER_SEC)
        .unwrap_or(0.0);
    let end = tl
        .EndTime()
        .map(|t| t.Duration as f64 / TICKS_PER_SEC)
        .unwrap_or(0.0);
    let start = tl
        .StartTime()
        .map(|t| t.Duration as f64 / TICKS_PER_SEC)
        .unwrap_or(0.0);
    // EndTime 绝对，Position 相对 StartTime；壁纸要的是「已经播了多少」
    ((pos - start).max(0.0), (end - start).max(0.0))
}

fn hstr(v: windows::core::Result<windows::core::HSTRING>) -> String {
    v.map(|s| s.to_string_lossy()).unwrap_or_default()
}

/// 封面缓存：key 为曲目标识，命中即复用上次编好的 data URL
#[derive(Default)]
struct ArtworkCache {
    key: String,
    url: String,
}

impl ArtworkCache {
    fn get(&mut self, title: &str, artist: &str, album: &str, album_artist: &str) -> Option<String> {
        let key = format!("{title}\u{1}{artist}\u{1}{album}\u{1}{album_artist}");
        if key != self.key {
            self.key = key;
            self.url = read_artwork(title, artist, album).unwrap_or_default();
        }
        if self.url.is_empty() {
            None
        } else {
            Some(self.url.clone())
        }
    }
}

/// 重新打开当前会话的封面流并编成 data URL。
/// 丢一次会话对象没意义（封面流每次读都要现开），所以直接重新取会话。
fn read_artwork(_title: &str, _artist: &str, _album: &str) -> Result<String, String> {
    let mgr = manager()?;
    let session = mgr.GetCurrentSession().map_err(err_str)?;
    let props = session
        .TryGetMediaPropertiesAsync()
        .map_err(err_str)?
        .get()
        .map_err(err_str)?;
    let reference: IRandomAccessStreamReference = match props.Thumbnail() {
        Ok(r) => r,
        Err(_) => return Err("该媒体没有封面".into()),
    };
    let bytes = read_stream(&reference)?;
    Ok(data_url(&bytes))
}

fn read_stream(reference: &IRandomAccessStreamReference) -> Result<Vec<u8>, String> {
    let stream = reference
        .OpenReadAsync()
        .map_err(err_str)?
        .get()
        .map_err(err_str)?;
    let size = stream.Size().map_err(err_str)?;
    if size == 0 {
        return Err("封面流为空".into());
    }
    if size > MAX_ARTWORK_BYTES {
        return Err(format!("封面过大（{size} 字节），跳过"));
    }
    let input: IInputStream = stream.cast().map_err(err_str)?;
    let reader = DataReader::CreateDataReader(&input).map_err(err_str)?;
    let loaded = reader.LoadAsync(size as u32).map_err(err_str)?.get().map_err(err_str)?;
    let mut buf = vec![0u8; loaded as usize];
    reader.ReadBytes(&mut buf).map_err(err_str)?;
    Ok(buf)
}

/// 字节魔数 → MIME（GSMTC 不给 content type，只能自己嗅探）
fn data_url(bytes: &[u8]) -> String {
    use base64::Engine;
    let mime = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/jpeg"
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{mime};base64,{b64}")
}
