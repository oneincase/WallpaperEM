//! KDE Plasma 壁纸插件的**帧文件通路**（契约见 WallpaperEM-Plasma `docs/engine-contract.md`）。
//!
//! 为什么是「文件 + 原子替换」而不是 socket / 共享内存：插件跑在 **plasmashell 进程内**，
//! 那里出问题 = 桌面出问题，所以插件侧只能做「只读文件 + 定时轮询」这种最小动作；
//! 文件也是跨进程、跨 Wayland/X11、跨用户权限的最小公分母。
//!
//! 本模块只做三件事，且完全不碰渲染管线：
//! 1. 按 [`ExportConfig::fps`] 周期调用现有的
//!    [`crate::system_wallpaper::capture_via_renderer`]（页面自抓帧，当前实现返回 JPEG 字节）；
//! 2. 解码 → SHA-256 → 与上一帧逐字节比较。没变就**不写盘、也不递增序号** ——
//!    插件据此零解码、零 CPU（契约 §3.4/§5 的「画面无变化时不写帧也不递增序号」）；
//! 3. 有变化才编码 PPM P6，先写 `<目标>.tmp-<pid>` 再 `rename`：同目录 rename 是原子的，
//!    插件任意时刻读到的都是完整帧，不会看到半张图。
//!
//! 抓帧很贵（一次 WebView 往返 + JPEG 编解码），所以宁可少抓也不排队：一次抓帧超过一个
//! tick 就直接等下一拍，不补跑（见 [`run_loop`] 的 `MissedTickBehavior::Delay`）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tokio::sync::watch;

/// 帧文件目录（tmpfs）：不磨损 SSD、延迟低，内存占用就是「一张图」。
pub const DEFAULT_DIR: &str = "/dev/shm/wallpaperem";
pub const DEFAULT_FRAME_NAME: &str = "frame.ppm";
pub const DEFAULT_SEQ_NAME: &str = "frame.seq";
/// 默认 20fps：契约建议 30，但一次抓帧要过 WebView 往返，20 已经足够顺滑且留出余量。
pub const DEFAULT_FPS: u32 = 20;
pub const MIN_FPS: u32 = 1;
pub const MAX_FPS: u32 = 60;

/// 单次抓帧的超时。页面侧 [`crate::content_server::request_capture`] 内部自己最多等 20s，
/// 这里必须更小：导出循环是「实时出帧」，卡满 20s 等于白掉几百帧。10s 还抓不到说明渲染器
/// 病了 —— 记一次错误、下一拍重试，比继续等有用。
const CAPTURE_TIMEOUT_MS: u64 = 10_000;

/// [`FrameExporter::stop`] 等待后台任务收尾的上界：一次抓帧最长 [`CAPTURE_TIMEOUT_MS`]，
/// 加上编码/落盘（1080p 约几十 ms）与清理的裕量。超时就硬中止任务（`JoinHandle::abort`）
/// —— `stop()` 是同步调用，绝不能因为渲染器僵死而挂住调用方。
const STOP_WAIT_MS: u64 = CAPTURE_TIMEOUT_MS + 2_000;

/// 帧导出配置。构建后由 [`FrameExporter::start`] 取快照，之后不再读。
#[derive(Clone, Debug)]
pub struct ExportConfig {
    pub dir: PathBuf,
    pub fps: u32,
    /// 抓帧时请求的最大宽度（0/None = 原尺寸）。1080p 原图走一次 HTTP 回传很贵。
    pub max_width: Option<u32>,
    /// 只抓这个 label 的窗口（None = 由 capture_via_renderer 自己挑当前壁纸窗口）。
    pub only_label: Option<String>,
    /// 抓帧期间把引擎自己的壁纸窗口整窗透明（Linux/X11 的 `_NET_WM_WINDOW_OPACITY`）。
    ///
    /// **给 Plasma 壁纸插件模式用**：插件负责把导出的帧画到桌面上，引擎窗口就得让位。
    /// 关键是「透明」而不是「隐藏」——2026-10-14 实测：窗口被取消映射后 WebKitGTK 会
    /// 停掉页面的 rAF，抓到的帧冻结在最后一帧（同场景 7 秒 140 帧里 139 帧逐字节相同）；
    /// 保持映射 + 整窗透明则照常出帧（同 7 秒 83 帧全部有变化）。
    pub hide_window: bool,
    /// 优先用 **X11 窗口抓帧**（GetImage 抓引擎自己的壁纸窗口），失败自动回退页面抓帧。
    ///
    /// 为什么需要它：Linux/WebKitGTK 上页面侧自抓帧（`drawImage(glCanvas)` 再
    /// `toDataURL`）会返回**全黑**（2026-10-14 在本机实测：同一窗口 X11 侧平均亮度 192，
    /// 页面侧 99.96% 纯黑；既有的 `wallpaper_screenshot` 同样受影响）。X11 侧抓的是窗口
    /// 后备存储里的真实像素，**整窗透明（`hide_window`）之后内容仍在**，所以插件模式
    /// 能一边让出桌面一边继续出帧。Wayland / 窗口还没建出来时回退到页面抓帧。
    pub window_capture: bool,
}

impl ExportConfig {
    /// dir 缺省 = DEFAULT_DIR，fps 缺省 = DEFAULT_FPS；fps 会截到 MIN_FPS..=MAX_FPS。
    pub fn new() -> Self {
        Self {
            dir: PathBuf::from(DEFAULT_DIR),
            fps: DEFAULT_FPS,
            max_width: None,
            only_label: None,
            hide_window: false,
            window_capture: false,
        }
    }

    pub fn with_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = dir.into();
        self
    }

    /// fps 越界就截到 [`MIN_FPS`]..=[`MAX_FPS`]：0 会让 tick 周期除零/空转，过大只是白烧
    /// CPU（抓一帧通常远慢于 16ms）。字段是 `pub`，直接改结构体也能绕过，所以
    /// [`FrameExporter::start`] 会再截一次。
    pub fn with_fps(mut self, fps: u32) -> Self {
        self.fps = clamp_fps(fps);
        self
    }

    pub fn with_max_width(mut self, max_width: Option<u32>) -> Self {
        self.max_width = max_width;
        self
    }

    pub fn with_label(mut self, label: Option<String>) -> Self {
        self.only_label = label;
        self
    }

    /// 插件模式：抓帧期间把引擎窗口整窗透明（见 [`ExportConfig::hide_window`]）。
    pub fn with_hide_window(mut self, hide: bool) -> Self {
        self.hide_window = hide;
        self
    }

    /// 优先 X11 窗口抓帧（见 [`ExportConfig::window_capture`]）。
    pub fn with_window_capture(mut self, on: bool) -> Self {
        self.window_capture = on;
        self
    }
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self::new()
    }
}

fn clamp_fps(fps: u32) -> u32 {
    fps.clamp(MIN_FPS, MAX_FPS)
}

/// 导出统计。`frames` = 成功抓取并解码的帧数，`written + skipped` 是其两个去向，
/// `errors` 是抓帧/解码/落盘失败的次数（与 `frames` 互斥）。
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ExportStats {
    /// 成功抓到的帧数
    pub frames: u64,
    /// 其中真正落盘的（内容有变化）
    pub written: u64,
    /// 与上一帧逐字节相同、省掉一次插件解码的
    pub skipped: u64,
    /// 抓帧/解码失败次数
    pub errors: u64,
    pub last_seq: u64,
    pub last_error: Option<String>,
    pub width: u32,
    pub height: u32,
    /// 最后一次落盘的帧字节数
    pub bytes: u64,
}

/// 导出状态快照（给 MCP 工具/设置面板看）。`label` = 最近一次真正抓到的窗口，
/// 一次都没抓到过时退回配置里的 `only_label`。
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExportStatus {
    pub running: bool,
    pub dir: String,
    pub frame_path: String,
    pub seq_path: String,
    pub fps: u32,
    pub label: Option<String>,
    /// 被设为整窗透明的引擎壁纸窗口数（0 = 没透明化；>0 = 插件模式，引擎自己不出图）
    pub hidden_windows: usize,
    /// 最近一次成功抓帧的来源：`"x11-window"`（X11 窗口抓帧）或 `"page"`（页面自抓帧）
    pub source: String,
    pub stats: ExportStats,
}

/// 后台任务与调用方共享的状态：任务写、[`FrameExporter::status`] 读。
#[derive(Default)]
struct Shared {
    stats: Mutex<ExportStats>,
    running: AtomicBool,
    label: Mutex<Option<String>>,
    /// 最近一次成功抓帧的来源（见 [`ExportStatus::source`]）
    source: Mutex<Option<String>>,
    /// 已被透明化的引擎壁纸窗口数（插件模式；0 = 还没成功或没开）
    hidden: AtomicUsize,
    exit: ExitFlag,
}

/// 任务退出标记。[`FrameExporter::stop`] 必须在**同步**上下文里有界等待，不能用
/// `tauri::async_runtime::block_on` —— 调用方可能就在同一个 async 运行时线程上，
/// 在那里 `block_on` 会 panic。Condvar 是纯同步的，任何线程都能安全等。
#[derive(Default)]
struct ExitFlag {
    done: Mutex<bool>,
    cv: Condvar,
}

impl ExitFlag {
    fn mark_done(&self) {
        match self.done.lock() {
            Ok(mut g) => *g = true,
            // 中毒只可能是任务里 panic 过；标记照样置上，等的一方不该因此 panic
            Err(p) => *p.into_inner() = true,
        }
        self.cv.notify_all();
    }

    /// 有界等待任务收尾。返回 true = 任务已退出（或锁中毒、无从判断，按已退出处理）。
    fn wait_exit(&self, bound: Duration) -> bool {
        let mut done = match self.done.lock() {
            Ok(g) => g,
            // 中毒 = 任务里 panic 过（标记照样会被置上），按已退出处理即可
            Err(_) => return true,
        };
        if *done {
            return true;
        }
        let deadline = std::time::Instant::now() + bound;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.cv.wait_timeout(done, left) {
                Ok((g, _)) => {
                    done = g;
                    if *done {
                        return true;
                    }
                }
                // 中毒：按已退出处理，绝不 panic
                Err(_) => return true,
            }
        }
    }
}

/// 帧导出器。`start` 起后台任务，`stop` 停并收尾，`status` 随时可读。
///
/// 故意**不**实现 `Drop`：停任务要等一次抓帧（最长 [`STOP_WAIT_MS`]），隐式在析构里
/// 阻塞线程比显式 `stop()` 更难排查；调用方必须显式停。
pub struct FrameExporter {
    cfg: ExportConfig,
    shared: Arc<Shared>,
    cancel_tx: watch::Sender<bool>,
    handle: tauri::async_runtime::JoinHandle<()>,
}

impl FrameExporter {
    /// 起一个后台任务开始导出。失败条件：目录不可创建/不可写。
    pub fn start(app: AppHandle, cfg: ExportConfig) -> Result<Self, String> {
        // 字段是 pub，可能绕过 `with_fps` 直接写 0，这里再截一次兜底
        let cfg = ExportConfig {
            fps: clamp_fps(cfg.fps),
            ..cfg
        };
        // 目录是**引擎**建的（契约 §3.1 权限 0755）：只在「这一轮刚建出来」时才设权限，
        // 免得把调用方自己指定的目录（可能是别的权限）改掉。失败只记不管 —— 权限不足
        // 时读得到的仍然读得到，下面的写探针会给出真正的原因。
        #[cfg(unix)]
        let existed = cfg.dir.is_dir();
        if let Err(e) = std::fs::create_dir_all(&cfg.dir) {
            return Err(format!("帧导出目录不可创建：{}（{e}）", cfg.dir.display()));
        }
        #[cfg(unix)]
        if !existed {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(&cfg.dir, std::fs::Permissions::from_mode(0o755))
            {
                tracing::debug!("帧导出目录权限设置为 0755 失败：{e}");
            }
        }
        // 可写性必须**现在**判定：等第一拍 publish 失败再报，调用方已经拿到 Ok 了，
        // 错误只能进 stats —— 而「目录不可写」是配置问题，必须当场说清。
        let probe = cfg
            .dir
            .join(format!(".frame-export-write-test-{}", std::process::id()));
        if let Err(e) = std::fs::write(&probe, b"") {
            return Err(format!("帧导出目录不可写：{}（{e}）", cfg.dir.display()));
        }
        let _ = std::fs::remove_file(&probe);

        let shared = Arc::new(Shared::default());
        shared.running.store(true, Ordering::SeqCst);
        // 插件模式：窗口可能还没建出来（壁纸是后应用的），这里先试一次，
        // run_loop 每拍还会补试（见那里的重试节流）—— 不成功不算启动失败。
        if cfg.hide_window {
            match crate::wallpaper::platform::set_wallpaper_windows_opacity(0.0) {
                Ok(n) if n > 0 => {
                    shared.hidden.store(n, Ordering::SeqCst);
                    tracing::info!("帧导出：引擎窗口已整窗透明（{n} 扇），交给壁纸插件显示帧");
                }
                Ok(_) => {
                    tracing::info!("帧导出：暂无可透明化的引擎窗口（hide_window 会在抓帧时重试）")
                }
                Err(e) => tracing::warn!("帧导出：整窗透明失败：{e}"),
            }
        }
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let handle =
            tauri::async_runtime::spawn(run_loop(app, cfg.clone(), shared.clone(), cancel_rx));
        tracing::info!(
            "帧导出启动：dir={} fps={} label={:?} maxWidth={:?} hideWindow={}",
            cfg.dir.display(),
            cfg.fps,
            cfg.only_label,
            cfg.max_width,
            cfg.hide_window
        );
        Ok(Self {
            cfg,
            shared,
            cancel_tx,
            handle,
        })
    }

    /// 停止并等后台任务收尾；顺手删掉可能残留的 `*.tmp-*` 临时文件。
    ///
    /// **别在 async 上下文里调用**：这里是同步的有界等待（Condvar），会把当前线程占住
    /// 最长 [`STOP_WAIT_MS`]。若它正好占着后台循环赖以被调度的那条 runtime 线程，循环
    /// 就没机会响应取消 —— 实测（Tauri 的 runtime）会**稳定等满 12s 上限**再去 `abort`。
    /// 异步路径请用 [`FrameExporter::stop_async`]。
    pub fn stop(self) {
        // sender 掉了也能唤醒：watch 的 changed() 会返回 Err，任务照样退出
        let _ = self.cancel_tx.send(true);
        if !self
            .shared
            .exit
            .wait_exit(Duration::from_millis(STOP_WAIT_MS))
        {
            tracing::warn!("帧导出：{STOP_WAIT_MS}ms 内未收尾，强制中止后台任务");
            self.handle.abort();
        }
        self.finish_stop();
    }

    /// 异步停止：等后台任务收尾，**不阻塞任何线程**。
    ///
    /// 与 [`FrameExporter::stop`] 的唯一区别是「等」的方式：`timeout(JoinHandle)` 让出
    /// runtime 线程，循环能正常被调度、看见取消标记后自己退出。stop 的收尾动作完全共用。
    pub async fn stop_async(self) {
        let Self {
            cfg,
            shared,
            cancel_tx,
            mut handle,
        } = self;
        let _ = cancel_tx.send(true);
        if tokio::time::timeout(Duration::from_millis(STOP_WAIT_MS), &mut handle)
            .await
            .is_err()
        {
            tracing::warn!("帧导出：{STOP_WAIT_MS}ms 内未收尾，强制中止后台任务");
            handle.abort();
        }
        finish_stop(&cfg, &shared);
    }

    /// 停止后的共同收尾：清临时文件、把整窗透明改回去、落 running=false。
    fn finish_stop(self) {
        finish_stop(&self.cfg, &self.shared);
    }

    /// 状态快照：只读共享状态，任何线程都可安全调用（不碰文件系统、不 await）。
    pub fn status(&self) -> ExportStatus {
        let stats = match self.shared.stats.lock() {
            Ok(s) => s.clone(),
            Err(p) => p.into_inner().clone(),
        };
        let label = match self.shared.label.lock() {
            Ok(l) => l.clone(),
            Err(p) => p.into_inner().clone(),
        }
        .or_else(|| self.cfg.only_label.clone());
        let source = match self.shared.source.lock() {
            Ok(s) => s.clone(),
            Err(p) => p.into_inner().clone(),
        };
        build_status(
            &self.cfg.dir,
            self.cfg.fps,
            label,
            stats,
            self.shared.running.load(Ordering::SeqCst),
            self.shared.hidden.load(Ordering::SeqCst),
            source,
        )
    }
}

/// 状态快照的统一组装点：`FrameExporter::status` 与从未启动过的 [`status_global`] 共用，
/// 保证两条路径给出的字段集与路径拼法完全一致（前端/MCP 只认一份形状）。
fn build_status(
    dir: &Path,
    fps: u32,
    label: Option<String>,
    stats: ExportStats,
    running: bool,
    hidden_windows: usize,
    source: Option<String>,
) -> ExportStatus {
    ExportStatus {
        running,
        dir: dir.to_string_lossy().into_owned(),
        frame_path: dir.join(DEFAULT_FRAME_NAME).to_string_lossy().into_owned(),
        seq_path: dir.join(DEFAULT_SEQ_NAME).to_string_lossy().into_owned(),
        fps,
        label,
        hidden_windows,
        // 一次都没抓到时按配置说「打算用哪条路」：面板上比空白更有信息量
        source: source.unwrap_or_else(|| "page".to_string()),
        stats,
    }
}

/// 停止后的共同收尾（同步 `stop` 与异步 `stop_async` 共用）：
/// 清临时文件 → 把整窗透明改回去 → 落 `running=false`。
///
/// `abort()` 可能正好落在 `rename` 之前、也可能落在写完临时文件之后，所以先扫一遍
/// 临时文件；透明化是可逆状态，必须在停的时候还回去 —— 留下一个全透明的壁纸窗口等于
/// 用户「壁纸不见了」，而插件可能已经不在显示帧。
fn finish_stop(cfg: &ExportConfig, shared: &Shared) {
    let swept = cleanup_tmp_files(&cfg.dir);
    if swept > 0 {
        tracing::info!("帧导出：停止时清理临时帧文件 {swept} 个");
    }
    let hidden = shared.hidden.swap(0, Ordering::SeqCst);
    if hidden > 0 {
        match crate::wallpaper::platform::set_wallpaper_windows_opacity(1.0) {
            Ok(n) => tracing::info!("帧导出：已恢复 {n} 扇引擎窗口的不透明度（停止）"),
            Err(e) => tracing::warn!("帧导出：恢复窗口不透明度失败：{e}"),
        }
    }
    shared.running.store(false, Ordering::SeqCst);
    tracing::info!("帧导出已停止：{}", cfg.dir.display());
}

// ---------- 进程级单例（MCP 工具的入口）----------
//
// `src-tauri/src/mcp/tools.rs` 的 frame_export_start/stop/status 是「一个进程只有一路
// 帧导出」的模型：插件读的就是固定路径下的 frame.ppm，同时跑两路只会互相覆盖、序号
// 乱跳。所以进程级槽位放在这里，工具层不必自己管生命周期。

/// 进程唯一的导出器槽位。用 `std::sync::Mutex` 而非 tokio 版：调用方是同步的 MCP
/// 处理函数，且持锁期间不做耗时操作（见下面每个函数）。
static GLOBAL: Mutex<Option<FrameExporter>> = Mutex::new(None);

/// 取全局槽位。中毒（某次持锁线程 panic）也要能继续用 —— 帧导出是旁路功能，
/// 一把中毒的锁绝不能升级成宿主 panic。
fn global_lock() -> std::sync::MutexGuard<'static, Option<FrameExporter>> {
    GLOBAL.lock().unwrap_or_else(|p| p.into_inner())
}

/// 从未启动过（或已停）时的状态：路径/帧率给默认值，统计全 0，字段和真跑起来时一致。
fn idle_status() -> ExportStatus {
    let cfg = ExportConfig::new();
    build_status(
        &cfg.dir,
        cfg.fps,
        cfg.only_label,
        ExportStats::default(),
        false,
        0,
        None,
    )
}

/// 起/换帧导出（进程内唯一）：已有导出先停掉再起新的，返回新导出的状态快照。
///
/// 失败（目录不可创建/不可写）时旧导出**已经停了**、槽位留空 —— 这是刻意的：
/// 「换参数重起但新参数不合法」时若还留着旧导出，调用方会以为新参数生效了。
///
/// 这是**异步**版本：换参数重起时不会占住 runtime 线程（见 [`FrameExporter::stop_async`]）。
pub async fn start_global_async(app: AppHandle, cfg: ExportConfig) -> Result<ExportStatus, String> {
    let previous = { global_lock().take() };
    if let Some(old) = previous {
        old.stop_async().await;
    }
    let exporter = FrameExporter::start(app, cfg)?;
    let status = exporter.status();
    *global_lock() = Some(exporter);
    Ok(status)
}

/// 停掉进程内的帧导出，返回停止时的最终统计；没在跑时返回空闲状态（幂等，不报错）。
pub fn stop_global() -> ExportStatus {
    let taken = { global_lock().take() };
    let Some(exporter) = taken else {
        return idle_status();
    };
    // 快照要在 stop(self) 之前取（self 会被消费掉）；running 按 stop() 的实际结果改写，
    // 而不是采信快照里那一刻（可能还是 true）的值。
    let mut status = exporter.status();
    exporter.stop();
    status.running = false;
    status
}

/// 异步版的 [`stop_global`]：不阻塞 runtime 线程。MCP 工具走这条。
pub async fn stop_global_async() -> ExportStatus {
    let taken = { global_lock().take() };
    let Some(exporter) = taken else {
        return idle_status();
    };
    let mut status = exporter.status();
    exporter.stop_async().await;
    status.running = false;
    status
}

/// 读当前状态；从未启动过 → 空闲状态（字段齐全，调用方不必处理 null）。
pub fn status_global() -> ExportStatus {
    let current = global_lock();
    match current.as_ref() {
        Some(e) => e.status(),
        None => idle_status(),
    }
}

/// 一帧的同步处理结果。只在阻塞线程池与循环之间传递，不对外。
enum FrameOutcome {
    Written {
        w: u32,
        h: u32,
        digest: [u8; 32],
        bytes: u64,
        seq: u64,
    },
    /// 与上一帧逐字节相同：不写盘、不递增序号
    Unchanged,
    Failed(String),
}

/// 一帧的同步处理：解码 → 变化检测 → 编码 → 原子落盘。只在阻塞线程池里跑。
fn process_frame(bytes: &[u8], dir: &Path, prev: Option<[u8; 32]>, seq: u64) -> FrameOutcome {
    // mime 是 opaque 的（当前是 image/jpeg，将来可能变），一律让 image crate 嗅探
    let img = match image::load_from_memory(bytes) {
        Ok(i) => i.to_rgba8(),
        Err(e) => return FrameOutcome::Failed(format!("解码失败: {e}")),
    };
    let (w, h) = img.dimensions();
    process_rgba(img.into_raw(), w, h, dir, prev, seq)
}

/// 已经是 RGBA8 的一帧：变化检测 → 编码 → 原子落盘（窗口抓帧直接走这里，省一次解码）。
fn process_rgba(
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    dir: &Path,
    prev: Option<[u8; 32]>,
    seq: u64,
) -> FrameOutcome {
    let need = (w as usize) * (h as usize) * 4;
    if w == 0 || h == 0 || rgba.len() != need {
        return FrameOutcome::Failed(format!(
            "帧像素长度与尺寸不符：{w}x{h} 需要 {need} 字节，实际 {}",
            rgba.len()
        ));
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&Sha256::digest(&rgba));
    if matches!(prev, Some(p) if same_frame(&p, &digest)) {
        return FrameOutcome::Unchanged;
    }
    let ppm = match encode_ppm_rgba(&rgba, w, h) {
        Ok(p) => p,
        Err(e) => return FrameOutcome::Failed(e),
    };
    let next_seq = seq + 1;
    match publish_frame(dir, DEFAULT_FRAME_NAME, DEFAULT_SEQ_NAME, &ppm, next_seq) {
        Ok(()) => FrameOutcome::Written {
            w,
            h,
            digest,
            bytes: ppm.len() as u64,
            seq: next_seq,
        },
        Err(e) => FrameOutcome::Failed(format!("写帧失败: {e}")),
    }
}

/// 一拍抓到的原始帧：页面回传的是编码图（PNG/JPEG），X11 窗口抓帧给的是裸 RGBA8。
enum FramePayload {
    Encoded(Vec<u8>),
    Rgba { rgba: Vec<u8>, w: u32, h: u32 },
}

impl FramePayload {
    fn process(self, dir: &Path, prev: Option<[u8; 32]>, seq: u64) -> FrameOutcome {
        match self {
            Self::Encoded(bytes) => process_frame(&bytes, dir, prev, seq),
            Self::Rgba { rgba, w, h } => process_rgba(rgba, w, h, dir, prev, seq),
        }
    }
}

/// 导出循环。用 `tauri::async_runtime`（本仓其它后台任务也都用它）而非裸 tokio，
/// 保证与应用共享同一个运行时。
async fn run_loop(
    app: AppHandle,
    cfg: ExportConfig,
    shared: Arc<Shared>,
    mut cancel: watch::Receiver<bool>,
) {
    // 第一拍之前先扫掉上次崩溃残留的临时文件（契约 §3.2：引擎退出/异常都要收自己的垃圾）
    let swept = cleanup_tmp_files(&cfg.dir);
    if swept > 0 {
        tracing::info!("帧导出：清理上次残留的临时帧文件 {swept} 个");
    }

    // fps 已截到 >= MIN_FPS，不会除零
    let period = Duration::from_millis((1000 / cfg.fps) as u64);
    let mut tick = tokio::time::interval(period);
    // Delay = 一次抓帧超时后不补跑堆积的 tick：壁纸要的是「最新一帧」，补跑只会拿过期
    // 画面把序号推快、把抓帧队列顶上。第一次 tick 立即返回，所以启动后马上抓一帧。
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut prev: Option<[u8; 32]> = None;
    let mut seq: u64 = 0;
    // 同一条错误只在第一次打日志：渲染器长期有病时 20fps 的错误洪泛会把日志淹掉
    // （插件契约 §4.2 对插件侧有同样要求，引擎侧照做）
    let mut logged: Option<String> = None;
    // 插件模式的透明化补试：窗口可能是 start 之后才建出来的（先开导出、后应用壁纸），
    // 每拍补试一次，成功后写进 shared.hidden 就不再枚举窗口。节流到 1s。
    let mut hide_next = std::time::Instant::now() - Duration::from_secs(2);
    // 心跳计时（仅用于日志，不影响导出节奏）
    let mut last_beat = std::time::Instant::now();

    loop {
        let iter_t0 = std::time::Instant::now();
        tokio::select! {
            _ = tick.tick() => {}
            _ = cancel.changed() => break,
        }
        if *cancel.borrow() {
            break;
        }

        if cfg.hide_window
            && shared.hidden.load(Ordering::SeqCst) == 0
            && hide_next.elapsed() >= Duration::from_secs(1)
        {
            hide_next = std::time::Instant::now();
            match crate::wallpaper::platform::set_wallpaper_windows_opacity(0.0) {
                Ok(n) if n > 0 => {
                    shared.hidden.store(n, Ordering::SeqCst);
                    tracing::info!("帧导出：引擎窗口已整窗透明（{n} 扇），交给壁纸插件显示帧");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("帧导出：整窗透明失败：{e}"),
            }
        }

        // 抓帧期间也响应停止：stop() 不必等满一次抓帧（丢掉的只是页面侧一次 POST）
        //
        // 两条抓帧路：插件模式优先 X11 窗口抓帧（Linux 上页面侧读回是黑帧，见
        // `ExportConfig::window_capture`），不可用时回退页面自抓帧。窗口抓帧是同步的
        // GetImage（1280x800 一次几 MB 内存拷贝），放阻塞线程池，别占 runtime 工作线程。
        let mut source = "page";
        let mut payload: Option<FramePayload> = None;
        if cfg.window_capture {
            let got = tokio::select! {
                r = tauri::async_runtime::spawn_blocking(
                    crate::wallpaper::platform::capture_wallpaper_window_rgba,
                ) => Some(r),
                _ = cancel.changed() => None,
            };
            match got {
                None => break,
                Some(Ok(Ok((rgba, w, h)))) => {
                    source = "x11-window";
                    payload = Some(FramePayload::Rgba {
                        rgba,
                        w: w as u32,
                        h: h as u32,
                    });
                }
                // 回退不是错误：Wayland 会话、窗口还没建出来都走这里（同一拍内改用页面路）
                Some(Ok(Err(e))) => {
                    if logged.as_deref() != Some(e.as_str()) {
                        tracing::info!("帧导出：窗口抓帧不可用，回退页面抓帧：{e}");
                    }
                }
                Some(Err(e)) => {
                    tracing::debug!("帧导出：窗口抓帧任务异常，回退页面抓帧：{e}");
                }
            }
        }
        if payload.is_none() {
            let captured = tokio::select! {
                r = crate::system_wallpaper::capture_via_renderer(
                    &app,
                    0,
                    0,
                    CAPTURE_TIMEOUT_MS,
                    cfg.max_width,
                    cfg.only_label.as_deref(),
                ) => Some(r),
                _ = cancel.changed() => None,
            };
            let Some(result) = captured else { break };
            match result {
                Ok((bytes, _mime, label)) => {
                    if let Ok(mut l) = shared.label.lock() {
                        *l = Some(label);
                    }
                    payload = Some(FramePayload::Encoded(bytes));
                }
                Err(e) => {
                    record_error(&shared, &mut logged, e);
                    continue;
                }
            }
        }
        let Some(payload) = payload else { break };
        if let Ok(mut s) = shared.source.lock() {
            *s = Some(source.to_string());
        }

        // 解码 + 哈希 + 编码 + 落盘是几十 ms 的同步 CPU/IO（1080p 的 SHA-256 要过 8MB），
        // 丢到阻塞线程池，别占着 tauri runtime 的工作线程 —— 同一 runtime 还跑着渲染器
        // 指令与命令响应。
        let dir = cfg.dir.clone();
        let prev_copy = prev;
        let current_seq = seq;
        // 这一拍同样挂在取消上：阻塞任务本身没法被中断（它会把这一帧处理完），但
        // **等它**必须能被取消 —— 否则停止要等这一拍跑完，实测在验证机上出现过
        // stop 卡满 12s 上限、循环来不及 mark_done 只能被 abort 的情况。
        let outcome = tokio::select! {
            r = tauri::async_runtime::spawn_blocking(move || {
                payload.process(&dir, prev_copy, current_seq)
            }) => match r {
                Ok(r) => r,
                Err(e) => FrameOutcome::Failed(format!("帧处理任务异常: {e}")),
            },
            _ = cancel.changed() => break,
        };

        match outcome {
            FrameOutcome::Written {
                w,
                h,
                digest,
                bytes: len,
                seq: new_seq,
            } => {
                // 只有整条链路（帧 + 序号）都成功才算「这一帧出来了」：中途失败时不能更新
                // prev，否则下一拍会以为画面没变、永远补不上那个序号
                prev = Some(digest);
                seq = new_seq;
                logged = None;
                if let Ok(mut s) = shared.stats.lock() {
                    s.frames += 1;
                    s.written += 1;
                    s.width = w;
                    s.height = h;
                    s.bytes = len;
                    s.last_seq = new_seq;
                    // 恢复正常后不留旧错：状态面板问的是「现在健不健康」
                    s.last_error = None;
                }
            }
            FrameOutcome::Unchanged => {
                if let Ok(mut s) = shared.stats.lock() {
                    s.frames += 1;
                    s.skipped += 1;
                }
            }
            FrameOutcome::Failed(msg) => record_error(&shared, &mut logged, msg),
        }
        // 一拍照理超过 1 秒就值得记一笔：它会把 stop() 的响应也拖慢（停止只能在
        // 「拍与拍之间」生效），也是排查“帧率上不去”的第一手线索。
        let spent = iter_t0.elapsed();
        if spent > Duration::from_secs(1) {
            tracing::warn!("帧导出：这一拍用了 {}ms（{}）", spent.as_millis(), source);
        }
        // 心跳（5s 一次）：帧导出是后台任务，出问题时要能从日志判断它是「在跑但没内容」
        // 还是「卡在某一拍」。带上 cancel 的当前值，停止路径卡住时一眼可辨。
        if iter_t0.duration_since(last_beat) >= Duration::from_secs(5) {
            last_beat = iter_t0;
            let st = shared.stats.lock().map(|s| s.clone()).unwrap_or_default();
            tracing::info!(
                "帧导出心跳：frames={} written={} skipped={} errors={} source={} cancelled={}",
                st.frames,
                st.written,
                st.skipped,
                st.errors,
                source,
                *cancel.borrow()
            );
        }
    }

    shared.running.store(false, Ordering::SeqCst);
    shared.exit.mark_done();
    tracing::info!("帧导出循环退出：{}", cfg.dir.display());
}

/// 记一次错误：同一条消息只打一次日志，统计照加。
fn record_error(shared: &Shared, logged: &mut Option<String>, msg: String) {
    if logged.as_deref() != Some(msg.as_str()) {
        tracing::warn!("帧导出失败: {msg}");
        *logged = Some(msg.clone());
    }
    if let Ok(mut s) = shared.stats.lock() {
        s.errors += 1;
        s.last_error = Some(msg);
    }
}

/// RGBA8 → PPM P6（`P6\n{w} {h}\n255\n` + RGB 原始字节，丢 alpha）；尺寸不符时报错。
///
/// 为什么 PPM：插件侧 `QQuickImage` 解 PPM 是 memcpy 级，引擎侧几乎零编码开销；
/// tmpfs 里多占的那点内存不值得为此加一层压缩（契约 §3.3 的取舍）。
/// 名字取契约里的 `encode_ppm`，[`encode_ppm_rgba`] 是它的别名（两个名字都在用，
/// 实现只有这一份，避免两处校验逻辑漂移）。
pub fn encode_ppm(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let need = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| format!("帧尺寸过大：{w}x{h}"))?;
    if rgba.len() != need {
        return Err(format!(
            "RGBA 长度与尺寸不符：{w}x{h} 需要 {need} 字节，实际 {}",
            rgba.len()
        ));
    }
    let pixels = need / 4;
    let header = format!("P6\n{w} {h}\n255\n");
    let mut out = Vec::with_capacity(header.len() + pixels * 3);
    out.extend_from_slice(header.as_bytes());
    // 长度已校验为 4 的整数倍，chunks_exact 不会留下尾巴
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    Ok(out)
}

/// [`encode_ppm`] 的别名（含 alpha 的入参形态，契约里两个名字都在用；行为完全一致，
/// 尺寸校验只有上面那一份）。
pub fn encode_ppm_rgba(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    encode_ppm(rgba, w, h)
}

/// 原子落盘：先写 `<dir>/<frame_name>.tmp-<pid>` 再 rename 到 `<dir>/<frame_name>`；
/// 成功后再原子写 `<dir>/<seq_name>`（十进制 seq + '\n'，同样 tmp+rename）。
///
/// 顺序不能反：插件「序号一变就去解码」，序号先落地会让它把**旧帧**当新帧再解一遍
/// （白烧一次解码，且破坏了契约 §3.4「序号变 = 新帧」的含义）。
///
/// rename 同一目录内才是原子的，所以临时文件必须和目标同目录（跨文件系统 rename 不原子）。
pub fn publish_frame(
    dir: &Path,
    frame_name: &str,
    seq_name: &str,
    ppm: &[u8],
    seq: u64,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    write_atomic(dir, frame_name, ppm)?;
    write_atomic(dir, seq_name, format!("{seq}\n").as_bytes())
}

/// 「写 `<name>.tmp-<pid>` → rename 到 `<name>`」。临时文件名带 pid 是为了区分别人
/// 正在写的临时文件（契约 §3.2）。
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    // 权限 0644（契约 §3.5）：插件与引擎通常同用户，显式设一次更稳（umask 可能更严）。
    // 失败不影响功能（读得到的仍然读得到），所以只记不管。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644));
    }
    if let Err(e) = std::fs::rename(&tmp, dir.join(name)) {
        // 写失败就把临时文件收掉，别给下一拍（以及插件目录）留垃圾
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// 删掉 `<dir>` 里本模块可能残留的临时帧文件（`frame.ppm.tmp-*` / `frame.seq.tmp-*`），
/// 返回删掉的数量。只在任务第一拍之前与 [`FrameExporter::stop`] 调用：正常路径下临时
/// 文件已被 rename 走，每帧扫目录纯属白花钱。
///
/// 它不区分 pid —— 同一目录同时跑第二个引擎实例时，可能删掉对方正在写的临时文件，
/// 对方那一次 rename 会失败、下一拍重写即可。契约要求「退出时清理自己的残留」，
/// 而没有可靠的跨进程「这个 pid 还活着吗」判定，宁可多删一次也不留垃圾。
fn cleanup_tmp_files(dir: &Path) -> u32 {
    let mut removed = 0u32;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let stale = [DEFAULT_FRAME_NAME, DEFAULT_SEQ_NAME]
            .iter()
            .any(|base| name.starts_with(&format!("{base}.tmp-")));
        if stale && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 变化检测：逐字节相同 = 同一帧。
///
/// **不做「近似相同」判定**：帧是 JPEG 回传后再解码的，编码噪声会让肉眼一样的画面逐字节
/// 不同；反过来，若把「差一两个 LSB」当成同一帧，插件就会继续显示与真实画面不一致的帧
/// —— 错帧比多一次解码糟得多。契约 §6 建议的下采样哈希能省 CPU，但会引入同样的误判
/// 风险，故不采纳；真正的省法是降低 fps 或在抓帧侧降 [`ExportConfig::max_width`]。
fn same_frame(prev: &[u8; 32], next: &[u8; 32]) -> bool {
    prev == next
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    /// 唯一临时目录（不用 tempfile：本仓没这个依赖，也不该为一个测试引入）。
    fn tmp_dir(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "wpem-frame-export-{tag}-{}-{n}-{stamp}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    /// PPM 头契约：`P6\n<w> <h>\n255\n`，总长 = 头 + w*h*3，且 alpha 被丢掉。
    #[test]
    fn ppm_header_and_alpha_drop() {
        // 2x1：红（不透明）+ 绿（半透明）
        let rgba = [255u8, 0, 0, 255, 0, 255, 0, 128];
        let ppm = encode_ppm_rgba(&rgba, 2, 1).expect("编码");
        let header = b"P6\n2 1\n255\n";
        assert_eq!(&ppm[..header.len()], header, r"头必须是 P6\n<w> <h>\n255\n");
        assert_eq!(ppm.len(), header.len() + 2 * 1 * 3, "总长 = 头 + w*h*3");
        assert_eq!(
            &ppm[header.len()..],
            &[255, 0, 0, 0, 255, 0],
            "逐像素 RGB 三连，alpha 丢弃"
        );
        // 别名行为一致
        assert_eq!(encode_ppm(&rgba, 2, 1).expect("别名编码"), ppm);
    }

    /// 尺寸与缓冲区不符必须报错（否则会写出一张尺寸对不上的残帧给插件）。
    #[test]
    fn ppm_rejects_wrong_rgba_len() {
        assert!(
            encode_ppm_rgba(&[0, 0, 0, 255], 2, 2).is_err(),
            "2x2 需要 16 字节"
        );
        assert!(encode_ppm(&[0, 0, 0, 255], 2, 2).is_err());
        // 0 尺寸 + 0 字节是合法的空帧，不该 panic
        assert_eq!(
            encode_ppm_rgba(&[], 0, 0).expect("空帧").len(),
            b"P6\n0 0\n255\n".len()
        );
    }

    /// 原子落盘：帧字节原样读回、序号是十进制 + 换行、**不留任何 `*.tmp-*`**。
    #[test]
    fn publish_frame_roundtrip_leaves_no_tmp() {
        let dir = tmp_dir("publish");
        let ppm = b"P6\n1 1\n255\n\xff\x00\x7f".to_vec();
        publish_frame(&dir, DEFAULT_FRAME_NAME, DEFAULT_SEQ_NAME, &ppm, 7).expect("落盘");

        assert_eq!(
            std::fs::read(dir.join(DEFAULT_FRAME_NAME)).expect("读帧"),
            ppm,
            "帧字节要能原样读回"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(DEFAULT_SEQ_NAME)).expect("读序号"),
            "7\n",
            "序号 = 十进制 + 换行"
        );

        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .expect("读目录")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "不该残留临时文件: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 连写两帧：序号文件内容必须递增（插件靠它决定重新解码）。
    #[test]
    fn publish_frame_seq_increments() {
        let dir = tmp_dir("seq");
        let ppm = b"P6\n1 1\n255\n\x01\x02\x03";
        publish_frame(&dir, DEFAULT_FRAME_NAME, DEFAULT_SEQ_NAME, ppm, 1).expect("第一帧");
        assert_eq!(
            std::fs::read_to_string(dir.join(DEFAULT_SEQ_NAME)).expect("序号"),
            "1\n"
        );
        publish_frame(&dir, DEFAULT_FRAME_NAME, DEFAULT_SEQ_NAME, ppm, 2).expect("第二帧");
        assert_eq!(
            std::fs::read_to_string(dir.join(DEFAULT_SEQ_NAME)).expect("序号"),
            "2\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 配置构造与 fps 截断。
    #[test]
    fn config_defaults_and_fps_clamp() {
        assert_eq!(ExportConfig::new().fps, DEFAULT_FPS);
        assert_eq!(
            ExportConfig::new().with_fps(0).fps,
            MIN_FPS,
            "0 会让 tick 除零"
        );
        assert_eq!(
            ExportConfig::new().with_fps(9999).fps,
            MAX_FPS,
            "过大只会白烧 CPU"
        );
        assert_eq!(ExportConfig::new().with_fps(30).fps, 30, "范围内不动");
        assert_eq!(ExportConfig::default().dir, PathBuf::from(DEFAULT_DIR));
        assert_eq!(
            ExportConfig::new().with_dir("/tmp/x").dir,
            PathBuf::from("/tmp/x")
        );
        assert_eq!(
            ExportConfig::new().with_max_width(Some(1280)).max_width,
            Some(1280)
        );
        assert_eq!(
            ExportConfig::new()
                .with_label(Some("wallpaper-1".into()))
                .only_label
                .as_deref(),
            Some("wallpaper-1")
        );
    }

    /// 变化检测是逐字节的：差 1 个 bit 也必须算「变了、要重写」。
    #[test]
    fn same_frame_compares_bytes_exactly() {
        let a = [7u8; 32];
        let b = [7u8; 32];
        let mut c = [7u8; 32];
        c[31] ^= 1;
        assert!(same_frame(&a, &b), "同一份摘要 = 没变");
        assert!(!same_frame(&a, &c), "1 bit 差异也必须重写");
    }

    /// 残留清理：只清本模块的 `frame.*.tmp-*`，正式帧与别人的临时文件都不碰。
    #[test]
    fn cleanup_removes_stale_tmp_only() {
        let dir = tmp_dir("cleanup");
        let frame = dir.join(DEFAULT_FRAME_NAME);
        std::fs::write(&frame, b"old frame").expect("预置帧");
        std::fs::write(dir.join(format!("{DEFAULT_FRAME_NAME}.tmp-99999")), b"half")
            .expect("预置残帧");
        std::fs::write(dir.join(format!("{DEFAULT_SEQ_NAME}.tmp-99999")), b"4")
            .expect("预置残序号");
        std::fs::write(dir.join("other.tmp-1"), b"unrelated").expect("无关临时文件");

        assert_eq!(cleanup_tmp_files(&dir), 2, "两个本模块的临时文件都要清掉");
        assert!(frame.exists(), "正式帧文件不能碰");
        assert!(dir.join("other.tmp-1").exists(), "别的临时文件不管");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 进程级单例的空闲态：没起过导出时 status/stop 也要给出字段齐全的合法 JSON，
    /// 且 `stop` 幂等不报错（测试不碰 AppHandle，所以只覆盖「没在跑」这条路径）。
    #[test]
    fn global_idle_status_is_well_formed() {
        let stopped = stop_global();
        assert!(!stopped.running, "没起过就不是 running");
        assert_eq!(stopped.dir, DEFAULT_DIR);
        assert_eq!(stopped.fps, DEFAULT_FPS);
        assert_eq!(stopped.label, None);
        assert_eq!(
            stopped.frame_path,
            format!("{DEFAULT_DIR}/{DEFAULT_FRAME_NAME}")
        );
        assert_eq!(
            stopped.seq_path,
            format!("{DEFAULT_DIR}/{DEFAULT_SEQ_NAME}")
        );
        assert_eq!(stopped.stats.frames, 0);
        assert_eq!(stopped.stats.written, 0);
        assert_eq!(stopped.stats.last_error, None);

        let status = status_global();
        assert!(!status.running);
        assert_eq!(status.stats.last_seq, 0);
        // 序列化给 MCP 工具用：字段名必须是 snake_case 全小写，别悄悄被改名
        let json = serde_json::to_value(&status).expect("状态可序列化");
        assert!(json.get("running").is_some());
        assert!(json.get("stats").is_some());
        assert!(json["stats"].get("last_error").is_some());
    }
}
