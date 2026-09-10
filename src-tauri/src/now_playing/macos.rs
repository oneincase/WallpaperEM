//! macOS 实现：MediaRemote 私有框架 → /usr/bin/perl adapter（mediaremote-adapter）。
//!
//! 自 macOS 15.4 起 mediaremoted 按调用进程的 bundle id 校验（硬编码 com.apple.
//! 前缀），非 Apple 进程拿到空字典。绕过办法是借 Apple 平台签名的 /usr/bin/perl
//! 加载 vendor/mediaremote-adapter（BSD-3-Clause，github.com/ungive/mediaremote-adapter）
//! 读全量数据；反向控制经同一 adapter 的 `send N` 一次性子进程。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tauri::{AppHandle, Manager};

use super::{
    MediaCommand, MediaShared, MediaSnapshot, STATE_PAUSED, STATE_PLAYING, STATE_STOPPED, STOPPING,
};

/// adapter 的换歌抖动抑制（毫秒）。换一首歌 MediaRemote 会连发数条通知
/// （先清空、再补齐元信息、最后补封面），不去抖会让壁纸看到中间的空态闪一下
const DEBOUNCE_MS: u32 = 120;

/// adapter 资产路径（perl 脚本 + framework）。dev 下在源码树，打包后在 Resources。
/// 解析一次后缓存：`send_command` 是每次点击都调的热路径，且路径不会变
static PATHS: OnceLock<Option<(PathBuf, PathBuf)>> = OnceLock::new();

fn adapter_paths(app: &AppHandle) -> Option<(PathBuf, PathBuf)> {
    PATHS.get_or_init(|| resolve_adapter_paths(app)).clone()
}

/// 已缓存的 adapter 路径（不需要 AppHandle 的调用方用，如内容服务器）。
/// 首次解析必须先经 [`start`] 或 [`adapter_paths`] 完成
fn cached_paths() -> Option<(PathBuf, PathBuf)> {
    PATHS.get().cloned().flatten()
}

fn resolve_adapter_paths(app: &AppHandle) -> Option<(PathBuf, PathBuf)> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        roots.push(dir.join("mediaremote-adapter"));
    }
    // dev：cargo 运行目录是 src-tauri
    roots.push(PathBuf::from("vendor/mediaremote-adapter"));
    roots.push(PathBuf::from("src-tauri/vendor/mediaremote-adapter"));

    for root in roots {
        let script = root.join("mediaremote-adapter.pl");
        let framework = root.join("MediaRemoteAdapter.framework");
        if script.is_file() && framework.is_dir() {
            return Some((script, framework));
        }
    }
    None
}

// 停机标志在公共层定义（STOPPING）：不置位的话 stream_loop 是死循环，
// 进程退出时 perl 子进程会被 launchd 收养成孤儿（PPID=1）常驻系统。

/// 记录当前 adapter 子进程的 pid，供停机时跨线程回收。
/// 用 pid 而不是共享 Child：Child::kill 要 &mut，跨线程共享得再套一层锁，
/// 而这里只需要「送一个 SIGTERM」，pid 足够。
static CHILD_PID: AtomicU32 = AtomicU32::new(0);

/// 请求停止媒体订阅并回收 adapter 子进程。App 退出前调用。
pub fn stop() {
    STOPPING.store(true, Ordering::SeqCst);
    let pid = CHILD_PID.swap(0, Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    // SIGTERM：adapter 是个 perl 脚本，默认处理即退出。不用 SIGKILL 是为了
    // 让它有机会撤掉 MediaRemote 订阅。
    // SAFETY: 只对本进程 spawn 出来的 pid 发信号；pid 已被 swap 取出，
    // 不会与重启路径重复 kill。
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    tracing::info!("now playing: adapter 子进程 {pid} 已收到 SIGTERM");
}

/// 启动常驻订阅线程（一次性保证由公共层 now_playing::start 负责）
pub fn start(app: &AppHandle) {
    let Some((script, framework)) = adapter_paths(app) else {
        tracing::warn!("now playing: adapter 资产缺失，媒体集成不可用");
        return;
    };
    reap_orphan_adapters(&script);
    let shared = super::shared();
    std::thread::Builder::new()
        .name("now-playing".into())
        .spawn(move || stream_loop(&script, &framework, &shared))
        .ok();
}

/// 回收上一轮运行泄漏的 adapter 孤儿进程。
///
/// `stop()` 只覆盖 `RunEvent::Exit` 这条正常退出路径。App 被 SIGKILL、崩溃、
/// 或开发时强杀 `tauri dev` 时走不到那里，perl 子进程就被 launchd 收养
/// （PPID=1）常驻，并继续持有 MediaRemote 订阅。启动时按脚本路径精确匹配
/// 清一次，保证「进程内只跑一条流」的前提在跨次运行后依然成立。
///
/// 按**完整脚本路径**匹配而不是进程名：同名脚本可能属于别的应用/别的构建，
/// 误杀会打断用户其它程序的媒体订阅。
fn reap_orphan_adapters(script: &Path) {
    let script = script.to_string_lossy();
    let me = std::process::id();
    let Ok(out) = Command::new("/bin/ps")
        .args(["-Ao", "pid=,ppid=,command="])
        .output()
    else {
        return;
    };
    let mut reaped = 0usize;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        if pid == me || !line.contains(script.as_ref()) || !line.contains("stream") {
            continue;
        }
        // 只收孤儿（PPID=1）。仍有活父进程的说明另有实例在正常使用，不碰。
        if ppid != "1" {
            continue;
        }
        // SAFETY: 目标由完整脚本路径 + 孤儿状态双重限定，且只发 SIGTERM。
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        reaped += 1;
    }
    if reaped > 0 {
        tracing::info!("now playing: 回收了 {reaped} 个上轮泄漏的 adapter 孤儿进程");
    }
}
fn stream_loop(script: &Path, framework: &Path, shared: &MediaShared) {
    let mut backoff_secs = 1u64;
    while !STOPPING.load(Ordering::Relaxed) {
        match spawn_stream(script, framework) {
            Ok(mut child) => {
                CHILD_PID.store(child.id(), Ordering::SeqCst);
                shared.available.store(true, Ordering::Relaxed);
                backoff_secs = 1;
                read_stream(&mut child, shared);
                CHILD_PID.store(0, Ordering::SeqCst);
                let _ = child.kill();
                let _ = child.wait();
                if STOPPING.load(Ordering::Relaxed) {
                    break;
                }
                tracing::warn!("now playing: adapter 流结束，{backoff_secs}s 后重启");
            }
            Err(e) => {
                tracing::warn!("now playing: adapter 启动失败（{e}）");
            }
        }
        // 起不来就别忙等：媒体集成不是关键路径，退避到 30s 上限
        std::thread::sleep(std::time::Duration::from_secs(backoff_secs));
        backoff_secs = (backoff_secs * 2).min(30);
    }
    shared.available.store(false, Ordering::Relaxed);
}

fn spawn_stream(script: &Path, framework: &Path) -> Result<Child, String> {
    Command::new("/usr/bin/perl")
        .arg(script)
        .arg(framework)
        .arg("stream")
        // --no-diff：每条都是全量快照。差量模式要在这边重建状态机，
        // 而全量下 publish 自带去重，代价只是多几 KB
        .arg("--no-diff")
        .arg(format!("--debounce={DEBOUNCE_MS}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())
}

fn read_stream(child: &mut Child, shared: &MediaShared) {
    let Some(stdout) = child.stdout.take() else {
        return;
    };
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(json) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // payload 缺失/空 = 无播放会话（流的第一行就是这种握手帧）
        let payload = json.get("payload").and_then(|p| p.as_object());
        let snap = match payload {
            Some(map) if !map.is_empty() => parse_payload(&Value::Object(map.clone())),
            _ => MediaSnapshot::default(),
        };
        shared.publish(snap);
    }
}

/// adapter payload → MediaSnapshot
fn parse_payload(p: &Value) -> MediaSnapshot {
    let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let f = |k: &str| p.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);

    let playing = p.get("playing").and_then(|v| v.as_bool()).unwrap_or(false);
    let title = s("title");
    let artist = s("artist");
    // 标题与艺人都空 = 没有真实会话（某些播放器退出后会留下空壳通知）
    let has_media = !title.is_empty() || !artist.is_empty();

    let thumbnail = match (
        p.get("artworkData").and_then(|v| v.as_str()),
        p.get("artworkMimeType").and_then(|v| v.as_str()),
    ) {
        (Some(data), mime) if !data.is_empty() => {
            let mime = mime.filter(|m| !m.is_empty()).unwrap_or("image/jpeg");
            format!("data:{mime};base64,{data}")
        }
        _ => String::new(),
    };

    MediaSnapshot {
        has_media,
        state: if !has_media {
            STATE_STOPPED
        } else if playing {
            STATE_PLAYING
        } else {
            STATE_PAUSED
        },
        title,
        artist,
        album: s("album"),
        // adapter 不单独给 albumArtist，WE 语义下缺省回落到 artist
        album_artist: {
            let aa = s("albumArtist");
            if aa.is_empty() {
                s("artist")
            } else {
                aa
            }
        },
        // elapsedTime 是 timestamp 那一刻的进度；播放中要按经过的真实时间外推，
        // 否则壁纸里的进度条会停在收到通知的位置不动
        position: extrapolate_position(
            f("elapsedTime"),
            p.get("timestamp"),
            playing,
            f("duration"),
        ),
        duration: f("duration"),
        has_thumbnail: !thumbnail.is_empty(),
        thumbnail,
        track_index: p.get("queueIndex").and_then(|v| v.as_i64()).unwrap_or(0),
    }
}

/// 按 timestamp 到现在的间隔外推播放进度（暂停时不外推），并夹在 [0, duration]
fn extrapolate_position(
    elapsed: f64,
    timestamp: Option<&Value>,
    playing: bool,
    duration: f64,
) -> f64 {
    let mut pos = elapsed;
    if playing {
        if let Some(ts) = timestamp.and_then(|v| v.as_str()).and_then(parse_iso8601) {
            if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
                let drift = now.as_secs_f64() - ts;
                // 负漂移（时钟回拨）与异常大的漂移都不采信
                if (0.0..600.0).contains(&drift) {
                    pos += drift;
                }
            }
        }
    }
    if duration > 0.0 {
        pos = pos.clamp(0.0, duration);
    } else if pos < 0.0 {
        pos = 0.0;
    }
    pos
}

/// 解析 adapter 的 `2026-09-08T04:47:39Z` 形态 UTC 时间戳 → Unix 秒。
/// 只认这一种固定形态，省一个日期库依赖
fn parse_iso8601(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    // 民用历 → Unix 天数（Howard Hinnant 的 days_from_civil）
    let y_adj = if mo <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + h * 3600 + mi * 60 + sec) as f64)
}

impl MediaCommand {
    fn id(self) -> u8 {
        match self {
            Self::Play => 0,
            Self::Pause => 1,
            Self::TogglePlayPause => 2,
            Self::NextTrack => 4,
            Self::PreviousTrack => 5,
        }
    }
}

/// 一次性子进程（adapter 的 send 是同步命令，不用常驻）
pub fn send_command(cmd: MediaCommand) -> Result<(), String> {
    let (script, framework) = cached_paths().ok_or("媒体控制不可用（adapter 资产缺失）")?;
    let out = Command::new("/usr/bin/perl")
        .arg(&script)
        .arg(&framework)
        .arg("send")
        .arg(cmd.id().to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if out.success() {
        Ok(())
    } else {
        Err(format!("媒体命令失败（exit {:?}）", out.code()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_real_adapter_payload() {
        // 取自真机 adapter stream 输出（Music.app 暂停态）
        let p = json!({
            "album": "十一月的萧邦",
            "artist": "周杰伦",
            "artworkData": "AAAA",
            "artworkMimeType": "image/jpeg",
            "bundleIdentifier": "com.apple.Music",
            "duration": 226.87,
            "elapsedTime": 2.027,
            "playing": false,
            "queueIndex": 2,
            "timestamp": "2026-09-08T04:47:39Z",
            "title": "夜曲"
        });
        let s = parse_payload(&p);
        assert!(s.has_media);
        assert_eq!(s.state, STATE_PAUSED, "playing=false → 暂停而非停止");
        assert_eq!(s.title, "夜曲");
        assert_eq!(s.artist, "周杰伦");
        assert_eq!(s.album, "十一月的萧邦");
        assert_eq!(s.album_artist, "周杰伦", "缺 albumArtist 回落 artist");
        assert_eq!(s.track_index, 2);
        assert!(s.has_thumbnail);
        assert_eq!(s.thumbnail, "data:image/jpeg;base64,AAAA");
        // 暂停态不外推进度
        assert!((s.position - 2.027).abs() < 1e-6);
    }

    #[test]
    fn empty_payload_means_no_media() {
        let s = parse_payload(&json!({}));
        assert!(!s.has_media);
        assert_eq!(s.state, STATE_STOPPED);
        assert!(s.thumbnail.is_empty());
        assert!(!s.has_thumbnail);

        // 只有空壳字段（播放器退出后的残留通知）同样视为无媒体
        let s = parse_payload(&json!({ "title": "", "artist": "", "playing": true }));
        assert!(!s.has_media);
        assert_eq!(s.state, STATE_STOPPED, "无会话时不能报播放中");
    }

    /// 播放中要按 timestamp 外推，否则壁纸进度条会卡住不动
    #[test]
    fn position_extrapolates_only_while_playing() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let ts = iso_of(now - 5.0);

        let playing = parse_payload(&json!({
            "title": "t", "playing": true, "elapsedTime": 10.0,
            "duration": 300.0, "timestamp": ts
        }));
        assert!(
            (playing.position - 15.0).abs() < 1.5,
            "播放中应外推约 5s，实际 {}",
            playing.position
        );

        let paused = parse_payload(&json!({
            "title": "t", "playing": false, "elapsedTime": 10.0,
            "duration": 300.0, "timestamp": ts
        }));
        assert!((paused.position - 10.0).abs() < 1e-6, "暂停不外推");
    }

    /// 外推不能越过总时长（壁纸拿 position/duration 算比例，越界会画出界）
    #[test]
    fn position_clamped_to_duration() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let s = parse_payload(&json!({
            "title": "t", "playing": true, "elapsedTime": 95.0,
            "duration": 100.0, "timestamp": iso_of(now - 60.0)
        }));
        assert_eq!(s.position, 100.0);
    }

    #[test]
    fn iso8601_roundtrip_and_rejects_garbage() {
        // 1970-01-01T00:00:00Z → 0
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0.0));
        // 2026-09-08T04:47:39Z（闰年规则 + 月份累加都要对）
        let v = parse_iso8601("2026-09-08T04:47:39Z").expect("应解析成功");
        // 用 SystemTime 交叉验证量级：2026 年应落在 1.78e9 附近
        assert!((1.7e9..1.9e9).contains(&v), "得到 {v}");
        assert_eq!(parse_iso8601("not-a-date"), None);
        assert_eq!(parse_iso8601(""), None);
        assert_eq!(parse_iso8601("2026-13-08T04:47:39Z"), None, "月份越界");
    }

    /// 相同快照不递增 seq —— 否则 SSE 会把上百 KB 的封面反复推给壁纸页
    #[test]
    fn publish_dedupes_identical_snapshots() {
        let shared = MediaShared::new();
        let snap = MediaSnapshot {
            has_media: true,
            title: "a".into(),
            ..Default::default()
        };
        shared.publish(snap.clone());
        let (seq1, got) = shared.snapshot();
        assert_eq!(got.title, "a");
        shared.publish(snap);
        let (seq2, _) = shared.snapshot();
        assert_eq!(seq1, seq2, "重复快照不应递增 seq");

        shared.publish(MediaSnapshot {
            has_media: true,
            title: "b".into(),
            ..Default::default()
        });
        let (seq3, _) = shared.snapshot();
        assert!(seq3 > seq2, "内容变化要递增 seq");
    }

    #[test]
    fn command_ids_match_mrcommand() {
        assert_eq!(MediaCommand::parse("play").unwrap().id(), 0);
        assert_eq!(MediaCommand::parse("pause").unwrap().id(), 1);
        assert_eq!(MediaCommand::parse("playPause").unwrap().id(), 2);
        assert_eq!(MediaCommand::parse("next").unwrap().id(), 4);
        assert_eq!(MediaCommand::parse("previous").unwrap().id(), 5);
        assert!(MediaCommand::parse("nope").is_none());
    }

    /// 播放中 snapshot() 要外推进度：adapter 只在系统通知时推送，不外推的话
    /// SSE 心跳会一直重复同一个 position，壁纸的进度条不动
    #[test]
    fn snapshot_advances_position_while_playing() {
        let shared = MediaShared::new();
        shared.publish(MediaSnapshot {
            has_media: true,
            state: STATE_PLAYING,
            position: 10.0,
            duration: 300.0,
            ..Default::default()
        });
        let (_, first) = shared.snapshot();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let (_, later) = shared.snapshot();
        assert!(
            later.position > first.position,
            "播放中应外推：{} → {}",
            first.position,
            later.position
        );

        // 暂停态不外推
        let paused = MediaShared::new();
        paused.publish(MediaSnapshot {
            has_media: true,
            state: STATE_PAUSED,
            position: 10.0,
            duration: 300.0,
            ..Default::default()
        });
        let (_, a) = paused.snapshot();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let (_, b) = paused.snapshot();
        assert_eq!(a.position, b.position, "暂停不该外推");
    }

    /// 外推同样不能越过总时长
    #[test]
    fn snapshot_position_clamped() {
        let shared = MediaShared::new();
        shared.publish(MediaSnapshot {
            has_media: true,
            state: STATE_PLAYING,
            position: 9.99,
            duration: 10.0,
            ..Default::default()
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        let (_, s) = shared.snapshot();
        assert_eq!(s.position, 10.0);
    }

    /// 测试辅助：Unix 秒 → adapter 的 ISO8601 形态
    fn iso_of(unix: f64) -> String {
        let secs = unix as i64;
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
        // days_from_civil 的逆运算
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
    }
}
