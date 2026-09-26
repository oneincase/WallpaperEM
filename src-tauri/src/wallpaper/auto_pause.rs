//! 自动暂停/恢复的判定核心（平台无关）。
//!
//! # 判据模型：**看得见就播**（纯桌面可见性，前台应用无关）
//!
//! 旧实现把暂停/恢复整个挂在「前台应用切换」通知上：切到非桌面应用暂停、
//! Finder 回到前台恢复。但 macOS 上「回到桌面」常常**不换前台应用**——
//! 最小化窗口、关掉最后一个窗口、F11 移开所有窗口、切到空桌面，前台仍停在
//! 原应用上，恢复信号永远不来，用户必须点一下桌面（激活 Finder）才恢复播放。
//!
//! 现在判据只回答一个问题：**这块屏的壁纸现在看得见吗**（逐屏独立判定）：
//!
//! | 条件（逐屏判定） | 动作 | 语义 |
//! |---|---|---|
//! | 该屏遮挡 < [`PAUSE_COVERED_MIN`]（露着） | Play | 看得见就播：半屏/小窗开着也照常播 |
//! | 该屏遮挡 ≥ [`PAUSE_COVERED_MIN`] | PauseSoft | 全屏应用/屏保/最大化窗口才暂停 |
//! | 显示器枚举为空（热插拔过渡） | Keep | 不动 |
//!
//! 前台是谁（Finder / 别的应用 / 本应用）**不影响播放**——它只用于指针注入
//! 门控（macos.rs / windows.rs），以及快照缺失平台的兜底。
//!
//! 快照（[`OcclusionSnapshot`]）拿不到时退化为纯前台语义 = 旧版行为
//! （Windows 暂未实现快照走这条，Linux 前台也未知 → 恒 Keep，开关不生效）。
//!
//! # 响应节奏
//!
//! - **前台切换（Hint）**：立即执行（对兜底语义是切应用立停立播的即时性）；
//! - **轮询（Poll，250ms）**：恢复立即执行（用户正等它）；PauseSoft 稳定
//!   2 tick 才动手，防全屏切换/最小化/调度动画的窗口闪烁来回抖；
//!   「暂停释放内存」下恢复要整窗重建，也要求稳定 2 tick。
//! - 所有单屏恢复路径汇到 [`resume_label`]，暂停路径汇到 [`auto_pause_enter_label`]；
//!   手动暂停优先（`pause_all` 清自动集合），开关关闭时自愈收尾自动暂停。

use super::platform;
use super::{auto_pause_enter_label, label_released, resume_label, WallpaperEngineState};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// 遮挡 ≥ 此值视为「壁纸看不见」→ 暂停。0.85 = 全屏应用/屏保（100%）与
/// 最大化窗口（macOS zoom 尺寸 ≈88%，只留菜单栏与程序坞边缘）都算看不见；
/// 半屏（~50%）、小窗仍照常播。菜单栏/程序坞等系统 UI 不计入遮挡（快照按
/// 窗口层过滤），故全屏应用正好到 1.0。
pub const PAUSE_COVERED_MIN: f64 = 0.85;

/// 轮询间隔：250ms。恢复的最坏延迟即由此兜底（前台没换的回桌面路径全靠它）；
/// CGWindowList 快照是单次系统调用，4Hz 成本可忽略。
const TICK_MS: u64 = 250;

/// PauseSoft 需要连续判定的 tick 数（250ms×2 = 500ms 稳定才暂停）
const PAUSE_STABLE_TICKS: u8 = 2;
/// 「暂停释放内存」下 Play 需要连续判定的 tick 数（重建整窗成本高，防 F11 抖）
const PLAY_STABLE_TICKS: u8 = 2;

/// 前台应用类型（平台后端 `platform::frontmost_kind` 提供）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontKind {
    /// 桌面（Finder / Explorer 的 Progman 等）
    Desktop,
    /// 本应用自己（点壁纸窗口 / 操作设置窗，判定表里再区分）
    SelfApp,
    /// 其他应用
    Other,
    /// 查询失败
    Unknown,
}

/// 桌面遮挡快照（平台后端提供；None = 平台未实现，走前台兜底语义）
pub struct OcclusionSnapshot {
    /// 每块活动显示器被盖住的比例（0.0–1.0，网格采样估算），键为稳定显示器 id。
    /// 只统计「盖壁纸」的窗口：普通应用窗口/屏保层；不含桌面元素、程序坞/菜单栏/
    /// 光标层等系统 UI，也不含本应用自己的窗口。
    pub coverages: Vec<(u32, f64)>,
}

/// 一次判定的结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 恢复播放（只恢复本功能自己挂的暂停，用户手动暂停不动）
    Play,
    /// 立即暂停（快照缺失平台的前台兜底语义：切到非桌面应用即时响应）
    Pause,
    /// 延迟暂停（壁纸看不见；轮询下需稳定 [`PAUSE_STABLE_TICKS`] 才动手）
    PauseSoft,
    /// 不动当前播放状态
    Keep,
}

/// 本次调用的来源：Hint=事件即时路径（前台切换/点桌面），Poll=轮询兜底
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Hint,
    Poll,
}

/// 单屏判定（纯函数）：**看得见就播** —— 该屏遮挡 < [`PAUSE_COVERED_MIN`] 就播，
/// 否则延迟暂停。多屏各自独立判定（一屏一个 label）。
pub fn evaluate_screen(covered: f64) -> Decision {
    if covered < PAUSE_COVERED_MIN {
        Decision::Play
    } else {
        Decision::PauseSoft
    }
}

/// 快照缺失平台的全局兜底判定（纯函数，旧版前台语义）：本应用前台且设置窗
/// 可见 = 在操作设置，保持中性；「点壁纸窗口」（本应用前台、无设置窗）按桌面语义。
pub fn evaluate_front(front: FrontKind, settings_visible: bool) -> Decision {
    if front == FrontKind::SelfApp && settings_visible {
        return Decision::Keep;
    }
    if matches!(front, FrontKind::Desktop | FrontKind::SelfApp) {
        Decision::Play
    } else if front == FrontKind::Other {
        Decision::Pause
    } else {
        Decision::Keep
    }
}

/// 稳定性计数（防抖，每 label 各自一份）。轮询路径用；Hint 路径直接执行但同样复位计数。
#[derive(Default)]
struct Stability {
    /// 连续多少 tick 判为 PauseSoft
    pause_soft: u8,
    /// 连续多少 tick 判为 Play
    play: u8,
}

impl Stability {
    fn reset(&mut self) {
        self.pause_soft = 0;
        self.play = 0;
    }
}

/// label -> 防抖计数（BTreeMap：new() 可作 static 初值；键数量=屏数，无性能诉求）
static STABILITY: Mutex<BTreeMap<String, Stability>> = Mutex::new(BTreeMap::new());

/// 事件即时路径（前台切换等）的入口：前台类型由事件自带（避免与
/// frontmostApplication 查询竞态），快照现场取。
pub fn recheck_with(app: &AppHandle, mode: Mode, front: FrontKind) {
    // 开关关着：只负责收尾 —— 外部直接改库/竞态遗留的自动暂停在此自愈，
    // 用户手动暂停不动（只动 auto_paused 集合内的）
    if !auto_pause_enabled(app) {
        STABILITY.lock().unwrap().clear();
        resume_auto_paused(app, "开关关闭收尾");
        return;
    }
    let snap = platform::occlusion_snapshot();
    match snap {
        Some(s) if !s.coverages.is_empty() => {
            // 按屏独立：每块屏各自「看得见就播」，互不牵连
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for (id, covered) in &s.coverages {
                let label = format!("wallpaper-{id}");
                seen.insert(label.clone());
                decide_and_apply(app, &label, evaluate_screen(*covered), mode);
            }
            // 断开屏的防抖残档顺手清掉
            STABILITY.lock().unwrap().retain(|k, _| seen.contains(k));
        }
        _ => {
            // 快照缺失（Windows/Linux）/显示器枚举过渡：全局前台兜底语义
            // （旧版行为），施加到所有在管 label
            let decision = evaluate_front(front, settings_window_visible(app));
            let labels: Vec<String> = app
                .try_state::<WallpaperEngineState>()
                .map(|st| st.windows.lock().unwrap().keys().cloned().collect())
                .unwrap_or_default();
            for label in labels {
                decide_and_apply(app, &label, decision, mode);
            }
        }
    }
}

/// 轮询兜底路径的入口：前台类型现场查。
pub fn recheck(app: &AppHandle, mode: Mode) {
    recheck_with(app, mode, platform::frontmost_kind(app));
}

/// 单 label 的防抖 + 落地（计数按 label 各自一份，屏间互不影响）。
fn decide_and_apply(app: &AppHandle, label: &str, decision: Decision, mode: Mode) {
    let mut map = STABILITY.lock().unwrap();
    let stab = map.entry(label.to_string()).or_default();
    match decision {
        Decision::Keep => stab.reset(),
        Decision::Play => {
            stab.pause_soft = 0;
            match mode {
                Mode::Hint => {
                    stab.play = 0;
                    apply_play(app, label);
                }
                Mode::Poll => {
                    if label_released(app, label) {
                        // 「暂停释放内存」：恢复 = 整窗重建，等稳定 2 tick 防抖
                        stab.play = stab.play.saturating_add(1);
                        if stab.play >= PLAY_STABLE_TICKS {
                            apply_play(app, label);
                        }
                    } else {
                        stab.play = 0;
                        apply_play(app, label);
                    }
                }
            }
        }
        Decision::Pause => {
            stab.reset();
            apply_pause(app, label);
        }
        Decision::PauseSoft => {
            stab.play = 0;
            stab.pause_soft = stab.pause_soft.saturating_add(1);
            if matches!(mode, Mode::Hint) || stab.pause_soft >= PAUSE_STABLE_TICKS {
                apply_pause(app, label);
            }
        }
    }
}

/// 恢复单屏「自动暂停」挂起的播放；用户手动暂停（集合外）不动。
fn apply_play(app: &AppHandle, label: &str) {
    let Some(st) = app.try_state::<WallpaperEngineState>() else {
        return;
    };
    let was_auto = st.auto_paused.lock().unwrap().remove(label);
    if was_auto {
        resume_label(app, label);
        tracing::info!("auto-pause: {label} 已恢复播放（壁纸可见）");
    }
}

/// 单屏挂上自动暂停。只在「开关开、全局未手动暂停」时动手；
/// [`super::auto_pause_enter_label`] 内部按「暂停释放内存」决定只停渲染
/// 还是连窗口一起销毁。
fn apply_pause(app: &AppHandle, label: &str) {
    if !auto_pause_enabled(app) {
        return;
    }
    let Some(st) = app.try_state::<WallpaperEngineState>() else {
        return;
    };
    if *st.paused.lock().unwrap() {
        // 全局手动暂停优先，不动
        return;
    }
    if st.auto_paused.lock().unwrap().contains(label) {
        return;
    }
    if auto_pause_enter_label(app, label).is_ok() {
        st.auto_paused.lock().unwrap().insert(label.to_string());
        tracing::info!("auto-pause: {label} 壁纸不可见，已自动暂停");
    }
}

/// 把自动挂的暂停全部恢复（开关关闭/点桌面收尾用）；用户手动暂停不动。
fn resume_auto_paused(app: &AppHandle, reason: &str) {
    let Some(st) = app.try_state::<WallpaperEngineState>() else {
        return;
    };
    let labels: Vec<String> = {
        let mut g = st.auto_paused.lock().unwrap();
        let v: Vec<String> = g.iter().cloned().collect();
        g.clear();
        v
    };
    for label in labels {
        resume_label(app, &label);
        tracing::info!("auto-pause: {label} 已恢复播放（{reason}）");
    }
}

/// 点桌面恢复：只恢复「当前看得见」的屏 —— 被全屏盖住的屏保持自动暂停
/// （按屏独立语义：交互意图是用桌面，不是替看不见的屏烧 GPU）。
/// 快照缺失时退回全局恢复。
pub fn resume_visible(app: &AppHandle) {
    let snap = platform::occlusion_snapshot();
    let Some(s) = snap else {
        resume_auto_paused(app, "点桌面");
        return;
    };
    for (id, covered) in &s.coverages {
        if *covered < PAUSE_COVERED_MIN {
            apply_play(app, &format!("wallpaper-{id}"));
        }
    }
}

/// 自动暂停的对账循环：250ms 一轮，只在「开关开 或 挂在自动暂停上」时做重活。
///
/// 这是判据模型的兜底层：窗口最小化动画结束、F11 移窗、切到空桌面、
/// 窗口被拖出屏幕…… 这些路径都不产生前台切换通知，但都会体现在窗口清单里。
/// 快照成本是一次 CGWindowList/EnumWindows 系统调用，低频且可忽略。
pub fn start_ticker(app: &AppHandle) {
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        tracing::debug!("auto-pause ticker started ({TICK_MS}ms)");
        loop {
            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
            // 单轮 panic 不能杀死对账任务（与壁纸监控同规格）
            let tick = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tick_once(&app2);
            }));
            if let Err(p) = tick {
                let msg = p
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| p.downcast_ref::<String>().map(|s| s.clone()))
                    .unwrap_or_else(|| "unknown panic".into());
                tracing::error!("auto-pause tick panicked: {msg}");
            }
        }
    });
}

fn tick_once(app: &AppHandle) {
    // 显示器睡眠期间播放状态由睡眠路径（monitor_tick）接管，别去抢
    if platform::display_asleep() {
        STABILITY.lock().unwrap().clear();
        return;
    }
    let auto_now = app
        .try_state::<WallpaperEngineState>()
        .map(|st| !st.auto_paused.lock().unwrap().is_empty())
        .unwrap_or(false);
    if !auto_now && !auto_pause_enabled(app) {
        STABILITY.lock().unwrap().clear();
        return;
    }
    recheck(app, Mode::Poll);
}

/// 主设置窗/壁纸属性窗是否可见（任一可见即认为用户正在操作本应用设置，
/// 此时自身前台化保持中性；都不可见说明自身前台化是「点击桌面壁纸」所致）
pub fn settings_window_visible(app: &AppHandle) -> bool {
    let visible = |label: &str| {
        app.get_webview_window(label)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false)
    };
    if visible("main") {
        return true;
    }
    app.webview_windows()
        .keys()
        .any(|label| label.starts_with("props-") && visible(label))
}

/// 设置开关：`wallpaper_auto_pause`，默认关。直读 DB（4Hz 轮询 + 事件路径都是
/// 低频读取、SQLite 亚毫秒；直读免去与设置页/托盘两侧的缓存同步）。
pub fn auto_pause_enabled(app: &AppHandle) -> bool {
    app.try_state::<std::sync::Arc<Mutex<rusqlite::Connection>>>()
        .and_then(|db| {
            db.lock()
                .ok()
                .and_then(|c| crate::db::get_setting(&c, "wallpaper_auto_pause"))
        })
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

/// 网格采样估算 `rects`（(x1,y1,x2,y2)）对 `screen`（(x,y,w,h)）的遮挡比例。
/// 采样而非精确并集：阈值判断（0.95）只要 ~1% 精度，采样对任意窗口重叠天然稳健。
/// 坐标系由调用方保证一致（macOS：CG 全局左上原点；Windows：物理虚拟桌面坐标）。
pub(crate) fn covered_ratio(rects: &[(f64, f64, f64, f64)], screen: (f64, f64, f64, f64)) -> f64 {
    const NX: usize = 40;
    const NY: usize = 24;
    if screen.2 <= 0.0 || screen.3 <= 0.0 {
        return 0.0;
    }
    let mut covered = 0usize;
    for gy in 0..NY {
        for gx in 0..NX {
            let px = screen.0 + (gx as f64 + 0.5) * screen.2 / NX as f64;
            let py = screen.1 + (gy as f64 + 0.5) * screen.3 / NY as f64;
            if rects
                .iter()
                .any(|(x1, y1, x2, y2)| px >= *x1 && px < *x2 && py >= *y1 && py < *y2)
            {
                covered += 1;
            }
        }
    }
    covered as f64 / (NX * NY) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 语义核心（2026-09-26 拍板「看得见就播」，按屏独立）：露着就播
    #[test]
    fn visible_wallpaper_plays() {
        assert_eq!(evaluate_screen(0.08), Decision::Play);
        assert_eq!(evaluate_screen(0.5), Decision::Play);
    }

    /// 最大化窗口（macOS zoom 尺寸 ≈88% 遮挡）也算看不见 → 暂停（阈值 0.85 用户拍板）
    #[test]
    fn maximized_window_pauses() {
        assert_eq!(evaluate_screen(0.88), Decision::PauseSoft);
        assert_eq!(evaluate_screen(0.99), Decision::PauseSoft);
    }

    /// 阈值边界：恰好 0.85 按「看不见」处理，0.84 还算看得见
    #[test]
    fn pause_threshold_boundary() {
        assert_eq!(evaluate_screen(0.85), Decision::PauseSoft);
        assert_eq!(evaluate_screen(0.84), Decision::Play);
    }

    /// 按屏独立：每块屏只按自己的遮挡判定，互不牵连（A 全屏盖死停 A，B 露着播 B）
    #[test]
    fn screens_are_independent() {
        assert_eq!(evaluate_screen(0.99), Decision::PauseSoft);
        assert_eq!(evaluate_screen(0.4), Decision::Play);
    }

    /// 兜底判定（快照缺失平台，旧版前台语义）：切到非桌面暂停、桌面/点壁纸恢复
    #[test]
    fn front_fallback_semantics() {
        assert_eq!(evaluate_front(FrontKind::Other, false), Decision::Pause);
        assert_eq!(evaluate_front(FrontKind::Desktop, false), Decision::Play);
        assert_eq!(evaluate_front(FrontKind::SelfApp, false), Decision::Play);
        assert_eq!(evaluate_front(FrontKind::Unknown, false), Decision::Keep);
        // 兜底语义里「本应用前台 + 设置窗可见」= 在操作设置，保持中性
        assert_eq!(evaluate_front(FrontKind::SelfApp, true), Decision::Keep);
    }

    /// 遮挡采样：整屏单窗全覆盖、半屏、屏外窗口
    #[test]
    fn covered_ratio_basics() {
        let screen = (0.0, 0.0, 100.0, 100.0);
        assert!((covered_ratio(&[(0.0, 0.0, 100.0, 100.0)], screen) - 1.0).abs() < 1e-9);
        let half = covered_ratio(&[(0.0, 0.0, 100.0, 50.0)], screen);
        assert!(half > 0.45 && half < 0.55, "半屏遮挡应≈0.5，实得 {half}");
        assert_eq!(covered_ratio(&[(200.0, 200.0, 300.0, 300.0)], screen), 0.0);
        assert_eq!(covered_ratio(&[], screen), 0.0);
    }
}
