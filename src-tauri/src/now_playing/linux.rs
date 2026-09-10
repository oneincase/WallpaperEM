//! Linux「正在播放」：MPRIS（D-Bus）轮询实现。
//!
//! MPRIS 是 Linux 桌面的媒体控制标准接口（org.mpris.MediaPlayer2.*），
//! 覆盖 Spotify、VLC、Firefox/Chromium、各种原生播放器 —— 对应 macOS 的
//! MediaRemote，且是公开稳定 API，不需要任何绕行。
//!
//! 实现选择**轮询**（1s 间隔）而不是 PropertiesChanged 信号订阅：
//! - 快照本来就带进度外推（MediaShared.snapshot 按 published_at 推进），
//!   1s 粒度的状态/换歌刷新对壁纸场景足够；
//! - 播放器集合动态变化（随时开关），信号方案要为每个 player 维护订阅，
//!   轮询只跟 find_active 选出的当前播放器打交道，状态机简单得多；
//! - mpris crate 是阻塞 API，与这里「单线程轮询」的形态天然匹配。
//!
//! 封面：mpris:artUrl 是 file:// 或 http(s)://，拉下来编 base64 拼成 data URL
//! （与 macOS adapter 的 artworkData 输出形态一致）。按 URL 缓存，只在换 URL
//! 时重新拉取，避免每秒重复下载。

use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::AppHandle;

use super::{
    MediaCommand, MediaShared, MediaSnapshot, STATE_PAUSED, STATE_PLAYING, STATE_STOPPED, STOPPING,
};

/// 状态轮询间隔
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// D-Bus 连接失败后的重连间隔（媒体集成非关键路径，不忙等）
const RECONNECT_INTERVAL: Duration = Duration::from_secs(10);
/// 封面拉取体积上限（防异常播放器塞来超大图）
const MAX_ARTWORK_BYTES: usize = 4 * 1024 * 1024;

/// 启动常驻轮询线程（一次性保证由公共层 now_playing::start 负责）
pub fn start(_app: &AppHandle) {
    let shared = super::shared();
    std::thread::Builder::new()
        .name("now-playing-mpris".into())
        .spawn(move || poll_loop(&shared))
        .ok();
}

/// 请求停止：轮询线程下一轮检查退出（无子进程可回收）
pub fn stop() {
    STOPPING.store(true, Ordering::SeqCst);
}

/// 反向控制：重新定位当前活动播放器并调用对应 MPRIS 方法。
/// 一次性连接（命令是低频用户操作，不值得常驻连接 + 追踪播放器生命周期）
pub fn send_command(cmd: MediaCommand) -> Result<(), String> {
    let finder = mpris::PlayerFinder::new().map_err(|e| format!("D-Bus 会话总线不可用: {e}"))?;
    let player = finder
        .find_active()
        .map_err(|_| "没有正在播放的媒体播放器".to_string())?;
    let r = match cmd {
        MediaCommand::Play => player.play(),
        MediaCommand::Pause => player.pause(),
        MediaCommand::TogglePlayPause => player.play_pause(),
        MediaCommand::NextTrack => player.next(),
        MediaCommand::PreviousTrack => player.previous(),
    };
    r.map_err(|e| format!("媒体命令失败: {e}"))
}

/// 封面 URL → data URL 的缓存（换 URL 才重拉）
struct ArtworkCache {
    url: String,
    data_url: String,
}

fn poll_loop(shared: &MediaShared) {
    // 封面 http(s) 拉取用的小型 runtime（轮询线程是裸线程，没有 tokio 上下文）
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok();
    let http = reqwest::Client::new();
    let mut finder: Option<mpris::PlayerFinder> = None;
    let mut artwork = ArtworkCache {
        url: String::new(),
        data_url: String::new(),
    };
    let mut last_connect_err = String::new();

    while !STOPPING.load(Ordering::Relaxed) {
        if finder.is_none() {
            match mpris::PlayerFinder::new() {
                Ok(f) => {
                    finder = Some(f);
                    shared.available.store(true, Ordering::Relaxed);
                    if !last_connect_err.is_empty() {
                        tracing::info!("now playing: D-Bus 重连成功");
                        last_connect_err.clear();
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg != last_connect_err {
                        tracing::warn!(
                            "now playing: D-Bus 连接失败（{msg}），{RECONNECT_INTERVAL:?} 后重试"
                        );
                        last_connect_err = msg;
                    }
                    shared.available.store(false, Ordering::Relaxed);
                    std::thread::sleep(RECONNECT_INTERVAL);
                    continue;
                }
            }
        }

        let f = finder.as_ref().expect("finder checked above");
        match f.find_active() {
            Ok(player) => match snapshot_from_player(&player, &mut artwork, rt.as_ref(), &http) {
                Ok(snap) => shared.publish(snap),
                Err(e) => {
                    // 查询失败多半是总线断开或播放器崩溃：重建连接
                    tracing::warn!("now playing: 查询失败（{e}），重建 D-Bus 连接");
                    finder = None;
                    shared.publish(MediaSnapshot::default());
                    std::thread::sleep(RECONNECT_INTERVAL);
                    continue;
                }
            },
            // 没有活动播放器：发布空快照（壁纸显示「无媒体」）
            Err(_) => shared.publish(MediaSnapshot::default()),
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    shared.available.store(false, Ordering::Relaxed);
}

/// 从当前活动播放器读取一次完整快照
fn snapshot_from_player(
    player: &mpris::Player,
    artwork: &mut ArtworkCache,
    rt: Option<&tokio::runtime::Runtime>,
    http: &reqwest::Client,
) -> Result<MediaSnapshot, String> {
    let meta = player.get_metadata().map_err(|e| e.to_string())?;
    let status = player.get_playback_status().map_err(|e| e.to_string())?;

    let title = meta.title().unwrap_or("").to_string();
    let artist = meta.artists().map(|v| v.join(", ")).unwrap_or_default();
    // 标题与艺人都空 = 没有真实会话（与 macOS parse_payload 同语义）
    let has_media = !title.is_empty() || !artist.is_empty();

    let album_artist = meta
        .album_artists()
        .map(|v| v.join(", "))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| artist.clone());

    // 封面：URL 变化才重拉；拉取失败只丢封面，不影响元信息
    let art_url = meta.art_url().unwrap_or("");
    if art_url != artwork.url {
        artwork.url = art_url.to_string();
        artwork.data_url = fetch_artwork(art_url, rt, http);
    }

    let playing = matches!(status, mpris::PlaybackStatus::Playing);
    Ok(MediaSnapshot {
        has_media,
        state: if !has_media {
            STATE_STOPPED
        } else if playing {
            STATE_PLAYING
        } else if matches!(status, mpris::PlaybackStatus::Paused) {
            STATE_PAUSED
        } else {
            STATE_STOPPED
        },
        title,
        artist,
        album: meta.album_name().unwrap_or("").to_string(),
        album_artist,
        // 进度外推由 MediaShared.snapshot 按 published_at 统一处理（1s 轮询 + 外推）
        position: player
            .get_position()
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
        duration: meta.length().map(|d| d.as_secs_f64()).unwrap_or(0.0),
        has_thumbnail: !artwork.data_url.is_empty(),
        thumbnail: artwork.data_url.clone(),
        // MPRIS 没有队列序号，用曲目号近似「换歌检测」语义
        track_index: meta.track_number().unwrap_or(0) as i64,
    })
}

/// 拉取封面并拼成 data URL；失败/不支持返回空串
fn fetch_artwork(
    art_url: &str,
    rt: Option<&tokio::runtime::Runtime>,
    http: &reqwest::Client,
) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    if art_url.is_empty() {
        return String::new();
    }
    let (bytes, mime) = if let Some(path) = art_url.strip_prefix("file://") {
        // file:// URL 可能带百分号编码（空格、非 ASCII 文件名）
        let path = url::Url::parse(art_url)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .unwrap_or_else(|| std::path::PathBuf::from(path));
        let bytes = read_capped(&path, MAX_ARTWORK_BYTES);
        let mime = match path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "png" => "image/png",
            "webp" => "image/webp",
            "gif" => "image/gif",
            _ => "image/jpeg",
        };
        (bytes, mime)
    } else if art_url.starts_with("http://") || art_url.starts_with("https://") {
        let Some(rt) = rt else { return String::new() };
        let bytes: Option<Vec<u8>> = rt.block_on(async {
            let resp = http.get(art_url).send().await.ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let b = resp.bytes().await.ok()?;
            (b.len() <= MAX_ARTWORK_BYTES).then(|| b.to_vec())
        });
        (bytes, "image/jpeg")
    } else {
        return String::new();
    };
    let Some(bytes) = bytes else {
        return String::new();
    };
    format!("data:{mime};base64,{}", STANDARD.encode(bytes))
}

/// 读文件（带体积上限；过大视为异常返回 None）
fn read_capped(path: &std::path::Path, cap: usize) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() as usize > cap {
        return None;
    }
    std::fs::read(path).ok()
}
