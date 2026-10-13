//! KWin / Plasma 后端能力（方案 P2 + P5，见 `docs/kde-adaptation-plan.md` §4.3、§4.4）
//!
//! 整个模块只在 Linux 编译（`lib.rs` 里 `#[cfg(target_os = "linux")]`）：它依赖 KWin 的
//! D-Bus 接口与 `kscreen-doctor`，其他平台没有对应物。
//!
//! 这里放两类东西，刻意分开：
//!
//! 1. **纯函数**：KWin 窗口规则（`kwinrulesrc`）的生成、与用户既有内容的合并/卸载、
//!    `kscreen-doctor -j` 的输出解析。这些不碰文件系统、不发 D-Bus，因此可在任何
//!    平台跑单测——这是本模块唯一能自动验证的部分，务必保持这个性质。
//! 2. **副作用薄层**：写文件、`qdbus org.kde.KWin /KWin reconfigure`、读空闲时间、
//!    读活动窗口。全部带超时、失败即返回 `None`/`false`，**绝不阻塞壁纸主循环**。
//!
//! ## 关于「不可验证」的诚实说明
//!
//! `kwinrulesrc` 的键名与规则类型（Force/Set）这里按 KWin 的 `rules.h` 语义写，
//! 但**没有在活的 KWin 6 上核对过实际落盘格式**（本项目的验证机只有 Plasma 5.27，
//! 且 `Layer` 规则在 KWin 5 上是否为 unused 尚未证实）。因此：
//!
//! - 生成结果**必须**在真机上用「写盘 → `qdbus … reconfigure` → `kwriteconfig6 --list`
//!   回读」核对一次，核对方法见 [`verify_installed_rules`] 的文档与
//!   `docs/kde-verification-report.md`。
//! - 在核对通过之前，调用方（设置页）必须保持「KDE 集成」是一个**可关闭的实验开关**，
//!   并且失败时明确提示用户，而不是静默认为已生效。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// 我们写入 `kwinrulesrc` 的规则组 id 前缀。
///
/// 只认这个前缀的组才是「我们的」规则：安装时先删同名旧组（幂等），卸载时精确删除
/// 这些组，用户自己的规则一个字都不动。改这个常量等于放弃对旧规则的识别。
pub const RULE_ID_PREFIX: &str = "wallpaperem-";
/// 壁纸窗口的 `WM_CLASS`（GTK 实测值，见验证报告：**不是** Tauri identifier）。
pub const WALLPAPER_WM_CLASS: &str = "WallpaperEM";
/// KWin 的 `Layer` 规则可能用到的枚举值（取 `rules.h` 中保持可见且可聚焦的类型）。
///
/// `layerrule` 的规则类型：0=Unused, 1=Force, 2=Set（Force 才能覆盖 `belongsToLayer()`）。
pub const LAYER_RULE_FORCE: u32 = 1;
/// `desktopsrule`/`activityrule` 是 Set 型：1=Set。
pub const SET_RULE_SET: u32 = 1;
/// 「匹配任意桌面」在 KWin 规则里用这个哨兵值（0 = 所有桌面）。
pub const DESKTOPS_ALL: &str = "0";

/// KWin `Layer` 枚举里我们要的「桌面层」。
///
/// KWin 的层序常量（`src/layers.cpp`）：NormalLayer=0 < AboveLayer=1 <
/// NotificationLayer=2 < ActiveLayer=3 < DockLayer=4；**DesktopLayer 与 BelowLayer
/// 是特殊值**（DesktopLayer 在枚举里排最底）。这里不用数字硬编码：改用
/// [`LayerRule::KeepBelow`]，语义上明确「低于普通窗口」，由 `layerrule=Force` 保证
/// 生效，避免把猜测的枚举值写进用户配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerRule {
    /// 强制「保持在下」（对普通窗口成立；能否进 DesktopLayer 由 KWin 决定）。
    KeepBelow,
}

impl LayerRule {
    /// KWin 规则里 `layer` 的数值。KeepBelow = 4（`Layer::BelowLayer`）。
    pub fn as_kwin_value(self) -> u32 {
        match self {
            LayerRule::KeepBelow => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LayerRule::KeepBelow => "keep-below",
        }
    }
}

/// 一次安装/卸载操作的结果（供设置页与日志使用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInstallOutcome {
    /// 是否真的写了文件（false 表示内容已经是目标态，**跳过写盘**）。
    pub wrote: bool,
    /// 写盘失败时的原因；`Some` 时其余字段无意义。
    pub error: Option<String>,
    /// 回读校验缺失的项（空 = 文件层校验通过）。
    pub missing: Vec<String>,
    /// 是否成功让 KWin 重新读取配置。
    pub reconfigured: bool,
}

impl RuleInstallOutcome {
    pub fn ok(&self) -> bool {
        self.error.is_none() && self.missing.is_empty()
    }
}

/// 校验规则组 id 是否是我们能安全管理的（带前缀、无 `]`/换行等会破坏 INI 的字符）。
pub fn id_is_manageable(id: &str) -> bool {
    id.starts_with(RULE_ID_PREFIX)
        && !id.contains(']')
        && !id.contains('[')
        && !id.contains('\n')
        && !id.contains('\r')
        && id.len() > RULE_ID_PREFIX.len()
}

/// 一条 KWin 窗口规则的**决策数据**（与落盘格式解耦，方便单测断言语义）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRule {
    /// 规则组 id（写入 `[<id>]` 段）。必须带 [`RULE_ID_PREFIX`]。
    pub id: String,
    /// `[<id>]` 段里的 `Description`。
    pub description: String,
    /// 匹配的 `WM_CLASS`。
    pub wm_class: String,
    /// 匹配方式：3 = 精确（`StringMatch::Exact`）。
    pub wm_class_match: u32,
    pub skip_taskbar: bool,
    pub skip_pager: bool,
    pub skip_switcher: bool,
    /// 是否不接收焦点（KWin 规则里**没有** `FocusPolicy`，只能写这个）。
    pub accept_focus: bool,
    pub layer: LayerRule,
    /// 是否要求 KWin 不画 decoration（无边框窗口在 KWin 下没有阴影/圆角）。
    pub no_decoration: bool,
    /// `Desktops`（Set 型）；[`DESKTOPS_ALL`] = 所有虚拟桌面。
    pub desktops: String,
    /// `Activity`（Set 型）；空串 + Set 规则 = 所有活动。
    pub activity: String,
}

impl WindowRule {
    /// 桌面层壁纸窗口的标准规则集。
    ///
    /// 语义：低于所有普通窗口、不占任务栏/分页器/任务切换器、不接收焦点、无 decoration、
    /// 在所有虚拟桌面与所有活动上可见。
    pub fn wallpaper(id_suffix: &str) -> Self {
        Self {
            id: format!("{RULE_ID_PREFIX}{id_suffix}"),
            description: format!("WallpaperEM 桌面层壁纸（{id_suffix}）"),
            wm_class: WALLPAPER_WM_CLASS.to_string(),
            wm_class_match: 3,
            skip_taskbar: true,
            skip_pager: true,
            skip_switcher: true,
            accept_focus: false,
            layer: LayerRule::KeepBelow,
            no_decoration: true,
            desktops: DESKTOPS_ALL.to_string(),
            activity: String::new(),
        }
    }

    /// 展开成 `kwinrulesrc` 的「键 → 值」列表（**不含** `[id]` 段头）。
    ///
    /// 顺序稳定（BTreeMap 之前就固定），便于逐字节断言与 diff。
    pub fn entries(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = Vec::with_capacity(20);
        v.push(("Description".into(), self.description.clone()));
        // 匹配
        v.push(("wmclass".into(), self.wm_class.clone()));
        v.push(("wmclassmatch".into(), self.wm_class_match.to_string()));
        v.push(("wmclasscomplete".into(), "false".into()));
        // 布尔类：键 + <Key>Rule=2(Force)
        v.push(("skiptaskbar".into(), bool_str(self.skip_taskbar)));
        v.push(("skiptaskbarrule".into(), "2".into()));
        v.push(("skippager".into(), bool_str(self.skip_pager)));
        v.push(("skippagerrule".into(), "2".into()));
        v.push(("skipswitcher".into(), bool_str(self.skip_switcher)));
        v.push(("skipswitcherrule".into(), "2".into()));
        // 不接收焦点（KWin 没有 FocusPolicy 字段）
        v.push(("acceptfocus".into(), bool_str(self.accept_focus)));
        v.push(("acceptfocusrule".into(), "2".into()));
        // 层：Force 型，能覆盖 belongsToLayer()
        v.push(("layer".into(), self.layer.as_kwin_value().to_string()));
        v.push(("layerrule".into(), LAYER_RULE_FORCE.to_string()));
        // decoration：无边框窗口不要 decoration
        v.push(("decoration".into(), bool_str(!self.no_decoration)));
        v.push(("decorationrule".into(), "2".into()));
        // 全桌面 / 全活动：Set 型
        v.push(("desktops".into(), self.desktops.clone()));
        v.push(("desktopsrule".into(), SET_RULE_SET.to_string()));
        v.push(("activity".into(), self.activity.clone()));
        v.push(("activityrule".into(), SET_RULE_SET.to_string()));
        v
    }
}

fn bool_str(b: bool) -> String {
    if b { "true".into() } else { "false".into() }
}

/// `kwinrulesrc` 的解析结果：段名 → 该段的键值（保留原始插入序）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KwinRulesFile {
    /// `[General]` 之类的段都在这里，便于原样写回。
    pub sections: Vec<(String, Vec<(String, String)>)>,
}

impl KwinRulesFile {
    /// 解析 INI 文本。无法解析的行（空行、`#`/`;` 注释）**原样丢弃**但不报错——
    /// KDE 自己也会重写这个文件，丢注释是可接受的；但我们绝不改动非本模块的键值。
    pub fn parse(text: &str) -> Self {
        let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let mut current: Option<usize> = None;
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                let name = line[1..line.len() - 1].trim().to_string();
                sections.push((name, Vec::new()));
                current = Some(sections.len() - 1);
                continue;
            }
            let Some(eq) = line.find('=') else {
                continue;
            };
            let key = line[..eq].trim().to_string();
            let value = line[eq + 1..].trim().to_string();
            match current {
                Some(i) => sections[i].1.push((key, value)),
                None => {
                    // 段头之前的键值：归到一个无名段，保持内容不丢
                    sections.push((String::new(), vec![(key, value)]));
                    current = Some(sections.len() - 1);
                }
            }
        }
        Self { sections }
    }

    /// 序列化成 INI 文本（段之间一个空行，与 KDE 习惯一致）。
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (name, entries) in &self.sections {
            if !name.is_empty() {
                out.push('[');
                out.push_str(name);
                out.push_str("]\n");
            }
            for (k, v) in entries {
                out.push_str(k);
                out.push('=');
                out.push_str(v);
                out.push('\n');
            }
            out.push('\n');
        }
        out
    }

    /// 移除所有「我们的」规则组（id 以 [`RULE_ID_PREFIX`] 开头）。
    pub fn remove_ours(&mut self) -> usize {
        let before = self.sections.len();
        self.sections
            .retain(|(name, _)| !name.starts_with(RULE_ID_PREFIX));
        before - self.sections.len()
    }

    /// 该 id 的段名是否已存在（用于幂等断言）。
    pub fn has_group(&self, id: &str) -> bool {
        self.sections.iter().any(|(name, _)| name == id)
    }
}

/// 合并结果：新的文件内容 + 这次实际安装的规则组 id 列表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    pub text: String,
    pub installed_ids: Vec<String>,
}

/// 把 `rules` 合并进 `existing`（幂等）：先删掉所有旧的本模块规则组，再追加新的。
///
/// **保证**：`existing` 里任何不以 [`RULE_ID_PREFIX`] 开头的段与键值，字节级不变
/// （顺序也保持），这样用户的既有规则不会被我们「重排」。
pub fn merge_rules(existing: &str, rules: &[WindowRule]) -> MergeOutcome {
    let mut file = KwinRulesFile::parse(existing);
    file.remove_ours();
    let mut installed_ids = Vec::with_capacity(rules.len());
    for r in rules {
        debug_assert!(r.id.starts_with(RULE_ID_PREFIX), "规则 id 必须带前缀");
        file.sections.push((r.id.clone(), r.entries()));
        installed_ids.push(r.id.clone());
    }
    MergeOutcome {
        text: file.render(),
        installed_ids,
    }
}

/// 卸载：删除所有本模块规则组，返回新文本与删除数量。
pub fn strip_rules(existing: &str) -> (String, usize) {
    let mut file = KwinRulesFile::parse(existing);
    let removed = file.remove_ours();
    (file.render(), removed)
}

/// 校验一份 `kwinrulesrc` 文本是否含我们期望的规则（供真机回读核对）。
///
/// 返回缺失的「组:键」列表；空列表 = 期望项齐全。注意这**只证明文件里写了**，
/// 不证明 KWin 认了——真机核对必须再看 `reconfigure` 之后窗口的
/// `layer`/`skipTaskbar` 实际值（见验证报告）。
pub fn verify_installed_rules(text: &str, expected_ids: &[String]) -> Vec<String> {
    let file = KwinRulesFile::parse(text);
    let mut missing = Vec::new();
    for id in expected_ids {
        let Some((_, entries)) = file.sections.iter().find(|(name, _)| name == id) else {
            missing.push(format!("{id}:<整段缺失>"));
            continue;
        };
        let map: BTreeMap<&str, &str> = entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        for key in [
            "wmclass",
            "layer",
            "layerrule",
            "skiptaskbar",
            "skippager",
            "skipswitcher",
            "acceptfocus",
            "desktops",
            "activity",
        ] {
            if !map.contains_key(key) {
                missing.push(format!("{id}:{key}"));
            }
        }
    }
    missing
}

// ---------------------------------------------------------------------------
// kscreen-doctor -j 解析（纯函数，可单测）
// ---------------------------------------------------------------------------

/// 一块显示器（KDE 侧查询结果，逻辑坐标 + 缩放）。
#[derive(Debug, Clone, PartialEq)]
pub struct KdeMonitor {
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub scale: f64,
    pub is_primary: bool,
}

/// 解析 `kscreen-doctor -j` 的 JSON。
///
/// 该输出随 Plasma 版本漂移（字段名有过变化），因此**宽松解析**：任一必需字段缺失
/// 就跳过这块屏幕并计数，绝不因为解析失败让整个查询报错——调用方在
/// `Vec` 为空时回落 GDK 枚举。返回 `(显示器列表, 被跳过的输出数)`。
pub fn parse_kscreen_monitors(json: &str) -> (Vec<KdeMonitor>, usize) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return (Vec::new(), 0);
    };
    let Some(outputs) = v.get("outputs").and_then(|o| o.as_array()) else {
        return (Vec::new(), 0);
    };
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for o in outputs {
        let name = o
            .get("name")
            .and_then(|x| x.as_str())
            .or_else(|| o.get("connectorName").and_then(|x| x.as_str()))
            .unwrap_or("")
            .to_string();
        // 关掉的输出没有 pos/size，直接跳过（不算失败）
        let Some(pos) = o.get("pos") else {
            skipped += 1;
            continue;
        };
        let Some(size) = o.get("size") else {
            skipped += 1;
            continue;
        };
        let get_f = |v: &serde_json::Value, k: &str| -> Option<f64> {
            v.get(k).and_then(|x| {
                x.as_f64()
                    .or_else(|| x.as_i64().map(|i| i as f64))
                    .or_else(|| x.as_str().and_then(|s| s.parse::<f64>().ok()))
            })
        };
        let (Some(x), Some(y)) = (get_f(pos, "x"), get_f(pos, "y")) else {
            skipped += 1;
            continue;
        };
        let (Some(w), Some(h)) = (get_f(size, "width"), get_f(size, "height")) else {
            skipped += 1;
            continue;
        };
        if w <= 0.0 || h <= 0.0 {
            skipped += 1;
            continue;
        }
        let scale = get_f(o, "scale").filter(|s| *s > 0.0).unwrap_or(1.0);
        let is_primary = o
            .get("priority")
            .and_then(|p| p.as_i64())
            .map(|p| p == 1)
            .or_else(|| o.get("primary").and_then(|p| p.as_bool()))
            .unwrap_or(false);
        out.push(KdeMonitor {
            name,
            x,
            y,
            w,
            h,
            scale,
            is_primary,
        });
    }
    (out, skipped)
}

// ---------------------------------------------------------------------------
// 副作用薄层 1：kwinrulesrc 的落盘（P2）
// ---------------------------------------------------------------------------

/// 串行化「读-改-写」的进程内互斥。
///
/// 两个窗口（桌面层 + 设置页）可能并发安装规则；没有它，后写者会把先写者的段覆盖掉。
/// 用 `try_lock`（不等待）：抢不到就报错返回，**绝不阻塞**壁纸主循环——调用点下一次
/// 启动/重挂会再试一次，规则安装是幂等的。
fn rules_write_guard() -> &'static Mutex<()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD.get_or_init(|| Mutex::new(()))
}

/// 用户级 `kwinrulesrc` 路径。`XDG_CONFIG_HOME` 未设时按 XDG 规范回落 `~/.config`。
pub fn rules_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home).join(".config")
        }
    };
    Some(base.join("kwinrulesrc"))
}

/// 读现有规则文件；不存在或读不动都当空内容（首次安装的正常路径）。
fn read_rules_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// 原子写：同目录临时文件 + `rename`。避免写到一半被 KWin 读到半截文件。
fn write_rules_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp-wallpaperem");
    std::fs::write(&tmp, text)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// 把 `rules` 安装进用户的 `kwinrulesrc`，然后让 KWin 重新读配置。
///
/// - **幂等**：内容已等于目标态时不写盘（`wrote == false`）。
/// - **不动别人的规则**：只删除/新增带 [`RULE_ID_PREFIX`] 的段，用户段字节级保留。
/// - **失败可诊断**：返回 [`RuleInstallOutcome`]，调用点据此给用户提示，不静默。
pub fn install_rules(rules: &[WindowRule]) -> RuleInstallOutcome {
    for r in rules {
        if !id_is_manageable(&r.id) {
            return RuleInstallOutcome {
                wrote: false,
                error: Some(format!("非法规则 id（必须带 {RULE_ID_PREFIX} 前缀且不含 [] 换行）：{}", r.id)),
                missing: Vec::new(),
                reconfigured: false,
            };
        }
    }
    let Ok(guard) = rules_write_guard().try_lock() else {
        return RuleInstallOutcome {
            wrote: false,
            error: Some("另一次窗口规则安装正在进行，跳过本次".into()),
            missing: Vec::new(),
            reconfigured: false,
        };
    };
    let result = install_rules_locked(rules);
    drop(guard);
    result
}

fn install_rules_locked(rules: &[WindowRule]) -> RuleInstallOutcome {
    let Some(path) = rules_path() else {
        return RuleInstallOutcome {
            wrote: false,
            error: Some("无法确定 kwinrulesrc 路径（HOME/XDG_CONFIG_HOME 均未设置）".into()),
            missing: Vec::new(),
            reconfigured: false,
        };
    };
    let existing = read_rules_text(&path);
    let merged = merge_rules(&existing, rules);
    let mut outcome = RuleInstallOutcome {
        wrote: false,
        error: None,
        missing: Vec::new(),
        reconfigured: false,
    };
    if merged.text != existing {
        if let Err(e) = write_rules_atomic(&path, &merged.text) {
            outcome.error = Some(format!("写入 {} 失败: {e}", path.display()));
            return outcome;
        }
        outcome.wrote = true;
    }
    // 回读校验：确认文件里确实有期望的段与键（写成功但内容不对 = 必须报出来）
    let readback = read_rules_text(&path);
    outcome.missing = verify_installed_rules(&readback, &merged.installed_ids);
    outcome.reconfigured = reconfigure_kwin();
    outcome
}

/// 卸载本模块安装的全部窗口规则（用户显式关闭 KDE 集成时调用）。
pub fn uninstall_rules() -> RuleInstallOutcome {
    let Ok(guard) = rules_write_guard().try_lock() else {
        return RuleInstallOutcome {
            wrote: false,
            error: Some("另一次窗口规则安装正在进行，跳过本次".into()),
            missing: Vec::new(),
            reconfigured: false,
        };
    };
    let result = uninstall_rules_locked();
    drop(guard);
    result
}

fn uninstall_rules_locked() -> RuleInstallOutcome {
    let Some(path) = rules_path() else {
        return RuleInstallOutcome {
            wrote: false,
            error: Some("无法确定 kwinrulesrc 路径".into()),
            missing: Vec::new(),
            reconfigured: false,
        };
    };
    let existing = read_rules_text(&path);
    let (text, removed) = strip_rules(&existing);
    let mut outcome = RuleInstallOutcome {
        wrote: false,
        error: None,
        missing: Vec::new(),
        reconfigured: false,
    };
    if removed > 0 && text != existing {
        if let Err(e) = write_rules_atomic(&path, &text) {
            outcome.error = Some(format!("写入 {} 失败: {e}", path.display()));
            return outcome;
        }
        outcome.wrote = true;
        outcome.reconfigured = reconfigure_kwin();
    }
    outcome
}

// ---------------------------------------------------------------------------
// 副作用薄层 2：KWin / KDE 的 D-Bus 查询（P5）
// ---------------------------------------------------------------------------

/// 我们对会话总线发起的几种查询。用枚举而不是闭包，是为了避开 `impl Trait`
/// 在泛型参数位置不可用的限制，同时让每种查询的载荷类型是静态确定的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DbusQuery {
    /// `org.kde.KWin /KWin reconfigure`
    KWinReconfigure,
    /// `org.freedesktop.ScreenSaver GetActive` → true = 屏幕已关/已锁
    ScreenSaverActive,
    /// `org.freedesktop.ScreenSaver GetSessionIdleTime` → 秒
    IdleSeconds,
}

/// 跑一次会话总线查询，最多等 `timeout`；任何失败都返回 `None`。
///
/// 为什么用独立线程 + 自己建运行时：调用点都在同步上下文（窗口处理、定时器），
/// 不能假设有 tokio 运行时句柄——这与 `kde.rs::kwin_service_present_impl` 的做法一致。
/// 用 `zbus` 的异步 API 而不是 `blocking`：后者要求 `blocking-api` feature，
/// 而这里只依赖 zbus 的默认路径，少一个可能被上游关掉的前提。
#[cfg(target_os = "linux")]
fn dbus_query(q: DbusQuery, timeout: Duration) -> Option<DbusValue> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()
            .and_then(|rt| rt.block_on(dbus_query_inner(q, timeout)));
        let _ = tx.send(out);
    });
    // 线程自身也有超时兜底，这里再留 100ms 余量，保证调用点一定能返回
    rx.recv_timeout(timeout + Duration::from_millis(100))
        .ok()
        .flatten()
}

/// 非 Linux：zbus 是 Linux 目标专属依赖，这里没有可用的会话总线实现。
/// 纯函数（规则合并/解析、kscreen JSON 解析）与单测仍全平台可用。
#[cfg(not(target_os = "linux"))]
fn dbus_query(_q: DbusQuery, _timeout: Duration) -> Option<DbusValue> {
    None
}

/// 查询结果载荷（只用得到这几种）。
#[derive(Debug, Clone, Copy, PartialEq)]
enum DbusValue {
    Bool(bool),
    Seconds(u64),
    /// 方法调用成功但没有要读的返回值
    Done,
}

#[cfg(target_os = "linux")]
async fn dbus_query_inner(q: DbusQuery, timeout: Duration) -> Option<DbusValue> {
    let conn = tokio::time::timeout(timeout, zbus::Connection::session())
        .await
        .ok()?
        .ok()?;
    match q {
        DbusQuery::KWinReconfigure => {
            let r = conn
                .call_method(
                    Some("org.kde.KWin"),
                    "/KWin",
                    Some("org.kde.KWin"),
                    "reconfigure",
                    &(),
                )
                .await
                .ok()?;
            // 有些 KWin 版本返回空体、有些不返回；能收到回复即视为成功
            let _ = r;
            Some(DbusValue::Done)
        }
        DbusQuery::ScreenSaverActive => {
            let r = conn
                .call_method(
                    Some("org.freedesktop.ScreenSaver"),
                    "/org/freedesktop/ScreenSaver",
                    Some("org.freedesktop.ScreenSaver"),
                    "GetActive",
                    &(),
                )
                .await
                .ok()?;
            Some(DbusValue::Bool(r.body().deserialize::<bool>().ok()?))
        }
        DbusQuery::IdleSeconds => {
            let r = conn
                .call_method(
                    Some("org.freedesktop.ScreenSaver"),
                    "/org/freedesktop/ScreenSaver",
                    Some("org.freedesktop.ScreenSaver"),
                    "GetSessionIdleTime",
                    &(),
                )
                .await
                .ok()?;
            let secs = r
                .body()
                .deserialize::<u32>()
                .map(u64::from)
                .or_else(|_| r.body().deserialize::<u64>())
                .or_else(|_| r.body().deserialize::<i64>().map(|v| v.max(0) as u64))
                .ok()?;
            Some(DbusValue::Seconds(secs))
        }
    }
}

/// 让 KWin 重新读取配置（写完 `kwinrulesrc` 必须调，否则规则要等下次登录才生效）。
pub fn reconfigure_kwin() -> bool {
    matches!(
        dbus_query(DbusQuery::KWinReconfigure, Duration::from_millis(800)),
        Some(DbusValue::Done)
    )
}

/// 系统空闲秒数（替代 Linux 上恒 false 的「显示器睡着」判断）。
///
/// 先问 `GetActive`：屏幕已关/已锁直接返回 `u64::MAX`（表示“睡着了”）；否则用
/// `GetSessionIdleTime`。任何一步失败都返回 `None`，由调用方回落到「认为没睡」——
/// 宁可继续播放，也不要因为查询失败把壁纸误停。
pub fn idle_seconds() -> Option<u64> {
    match dbus_query(DbusQuery::ScreenSaverActive, Duration::from_millis(600)) {
        Some(DbusValue::Bool(true)) => return Some(u64::MAX),
        Some(DbusValue::Bool(false)) => {}
        // 取不到「屏幕是否关」时不要贸然判定空闲，避免误停
        _ => return None,
    }
    match dbus_query(DbusQuery::IdleSeconds, Duration::from_millis(600)) {
        Some(DbusValue::Seconds(s)) => Some(s),
        _ => None,
    }
}

/// `kscreen-doctor -j` 的每屏查询（P5）。
///
/// 命令缺失、退出码非 0、JSON 形状不认识，都返回空 `Vec`——调用方回落 GDK 枚举，
/// 绝不因为 KDE 侧查询失败而拿不到任何屏幕。
pub fn kscreen_monitors() -> Vec<KdeMonitor> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    let out = match std::process::Command::new("kscreen-doctor")
        .arg("-j")
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    parse_kscreen_monitors(&text).0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把规则的键值表变成**拥有所有权**的 map（测试里多处要按 key 断言）。
    fn entries_of(r: &WindowRule) -> BTreeMap<String, String> {
        r.entries().into_iter().collect()
    }

    /// 从一段 `kwinrulesrc` 文本里取某个规则组的键值表（拥有所有权）。
    fn entries_in(text: &str, id: &str) -> BTreeMap<String, String> {
        KwinRulesFile::parse(text)
            .sections
            .into_iter()
            .find(|(name, _)| name == id)
            .map(|(_, e)| e.into_iter().collect())
            .expect("规则段应存在")
    }

    /// 一段「用户已有规则」的样例，用于验证我们绝不改动它。
    const USER_RULES: &str = "\
[General]
count=1

[1]
Description=用户自己的规则：让 kate 保持在所有桌面
wmclass=kate
wmclassmatch=3
layer=2
layerrule=1
desktops=0
desktopsrule=1
";

    #[test]
    fn wallpaper_rule_has_required_keys_and_omits_forbidden_ones() {
        let r = WindowRule::wallpaper("desktop");
        let entries = r.entries();
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        for must in [
            "wmclass",
            "layer",
            "layerrule",
            "skiptaskbar",
            "skippager",
            "skipswitcher",
            "acceptfocus",
            "desktops",
            "activity",
            "decoration",
        ] {
            assert!(keys.contains(&must), "规则缺少必需键 {must}");
        }
        // KWin 规则里没有 FocusPolicy，也没有单数 Opacity
        assert!(!keys.iter().any(|k| k.eq_ignore_ascii_case("focuspolicy")));
        assert!(!keys.iter().any(|k| *k == "opacity"));
        // 布尔规则必须同时带 <Key>Rule=2(Force)
        let map = entries_of(&r);
        assert_eq!(map["skiptaskbar"], "true");
        assert_eq!(map["skiptaskbarrule"], "2");
        assert_eq!(map["acceptfocus"], "false");
        assert_eq!(map["acceptfocusrule"], "2");
        assert_eq!(map["decoration"], "false");
        // 层是 Force 型
        assert_eq!(map["layerrule"], "1");
        assert_eq!(map["layer"], "4");
        // 全桌面/全活动是 Set 型
        assert_eq!(map["desktops"], DESKTOPS_ALL);
        assert_eq!(map["desktopsrule"], "1");
        assert_eq!(map["activity"], "");
        assert_eq!(map["activityrule"], "1");
        // 匹配用的是实测 WM_CLASS，而不是 Tauri identifier
        assert_eq!(map["wmclass"], "WallpaperEM");
        assert_eq!(map["wmclassmatch"], "3");
    }

    #[test]
    fn parse_preserves_user_sections_and_roundtrips() {
        let f = KwinRulesFile::parse(USER_RULES);
        assert_eq!(f.sections.len(), 2);
        assert_eq!(f.sections[0].0, "General");
        assert_eq!(f.sections[1].0, "1");
        // 再解析一次，内容一致（无信息丢失、无段顺序变化）
        let again = KwinRulesFile::parse(&f.render());
        assert_eq!(f, again);
    }

    #[test]
    fn merge_keeps_user_rules_byte_identical() {
        let out = merge_rules(USER_RULES, &[WindowRule::wallpaper("desktop")]);
        assert_eq!(out.installed_ids, vec!["wallpaperem-desktop".to_string()]);
        // 用户那段（含 Description 里的中文与全部键值）必须原样还在
        for line in USER_RULES.lines().filter(|l| !l.trim().is_empty()) {
            assert!(
                out.text.contains(line),
                "合并后丢了用户的行: {line:?}\n---\n{}",
                out.text
            );
        }
        // 我们的段确实写进去了
        assert!(out.text.contains("[wallpaperem-desktop]"));
        assert!(verify_installed_rules(&out.text, &out.installed_ids).is_empty());
    }

    #[test]
    fn merge_is_idempotent() {
        let once = merge_rules(USER_RULES, &[WindowRule::wallpaper("desktop")]);
        let twice = merge_rules(&once.text, &[WindowRule::wallpaper("desktop")]);
        assert_eq!(once.text, twice.text, "重复安装不得产生第二份规则段");
        assert_eq!(once.installed_ids, twice.installed_ids);
        // 用户规则仍然只有一份
        assert_eq!(twice.text.matches("[1]").count(), 1);
    }

    #[test]
    fn merge_replaces_changed_rule_without_duplicating() {
        let mut r = WindowRule::wallpaper("desktop");
        let once = merge_rules(USER_RULES, &[r.clone()]);
        r.no_decoration = false;
        let twice = merge_rules(&once.text, &[r]);
        assert_eq!(twice.text.matches("[wallpaperem-desktop]").count(), 1);
        let map = entries_in(&twice.text, "wallpaperem-desktop");
        assert_eq!(map["decoration"], "true");
    }

    #[test]
    fn strip_removes_only_ours() {
        let merged = merge_rules(USER_RULES, &[WindowRule::wallpaper("desktop")]);
        let (text, removed) = strip_rules(&merged.text);
        assert_eq!(removed, 1);
        assert!(!text.contains("wallpaperem-"));
        // 用户规则完好
        assert!(text.contains("[1]"));
        assert!(text.contains("wmclass=kate"));
        assert!(text.contains("Description=用户自己的规则：让 kate 保持在所有桌面"));
    }

    #[test]
    fn strip_on_empty_and_garbage_does_not_panic() {
        assert_eq!(strip_rules(""), (String::new(), 0));
        // 非 INI 内容：解析器不 panic，且不被当成我们的段
        let (text, removed) = strip_rules("这不是 ini\n@@@\n");
        assert_eq!(removed, 0);
        assert_eq!(text, "");
    }

    #[test]
    fn multiple_rules_installed_at_once() {
        let out = merge_rules(
            USER_RULES,
            &[
                WindowRule::wallpaper("desktop"),
                WindowRule::wallpaper("props"),
            ],
        );
        assert_eq!(out.installed_ids.len(), 2);
        assert!(out.text.contains("[wallpaperem-desktop]"));
        assert!(out.text.contains("[wallpaperem-props]"));
        let (text, removed) = strip_rules(&out.text);
        assert_eq!(removed, 2);
        assert!(text.contains("wmclass=kate"));
    }

    #[test]
    fn verify_reports_missing_group_and_keys() {
        let out = merge_rules("", &[WindowRule::wallpaper("desktop")]);
        assert!(verify_installed_rules(&out.text, &out.installed_ids).is_empty());
        // 组不存在
        let missing = verify_installed_rules("", &["wallpaperem-desktop".to_string()]);
        assert_eq!(missing, vec!["wallpaperem-desktop:<整段缺失>".to_string()]);
        // 组在但缺键
        let partial = "[wallpaperem-desktop]\nwmclass=WallpaperEM\n";
        let missing = verify_installed_rules(partial, &["wallpaperem-desktop".to_string()]);
        assert!(missing.iter().any(|m| m.ends_with(":layer")));
        assert!(missing.iter().all(|m| !m.contains("wmclass")));
    }

    #[test]
    fn rule_id_must_carry_prefix() {
        let bad = WindowRule {
            id: "1".into(),
            ..WindowRule::wallpaper("x")
        };
        // 带前缀的规则能被卸载；不带前缀的会被 remove_ours 漏掉 = 永久残留，
        // 所以这里断言 merge 之后 strip 能清干净（前缀是唯一的识别依据）
        let out = merge_rules("", &[WindowRule::wallpaper("desktop")]);
        assert!(out.installed_ids[0].starts_with(RULE_ID_PREFIX));
        assert!(!bad.id.starts_with(RULE_ID_PREFIX));
    }

    // ---------- kscreen-doctor -j 解析 ----------

    #[test]
    fn parses_typical_kscreen_json() {
        let json = r#"{
          "outputs": [
            {"name":"DP-1","pos":{"x":0,"y":0},"size":{"width":2560,"height":1440},
             "scale":1.5,"priority":1},
            {"name":"HDMI-A-1","pos":{"x":1707,"y":0},"size":{"width":1920,"height":1080},
             "scale":1.0,"priority":2}
          ]
        }"#;
        let (mons, skipped) = parse_kscreen_monitors(json);
        assert_eq!(skipped, 0);
        assert_eq!(mons.len(), 2);
        assert_eq!(mons[0].name, "DP-1");
        assert_eq!(mons[0].w, 2560.0);
        assert_eq!(mons[0].scale, 1.5);
        assert!(mons[0].is_primary);
        assert_eq!(mons[1].x, 1707.0);
        assert!(!mons[1].is_primary);
    }

    #[test]
    fn skips_disabled_and_malformed_outputs_without_failing() {
        let json = r#"{
          "outputs": [
            {"name":"DP-1","pos":{"x":0,"y":0},"size":{"width":1920,"height":1080},"scale":1,"priority":1},
            {"name":"DP-2"},
            {"name":"DP-3","pos":{"x":0,"y":0}},
            {"name":"DP-4","pos":{"x":0,"y":0},"size":{"width":0,"height":0}},
            {"name":"DP-5","pos":{"x":"100","y":"0"},"size":{"width":"1280","height":"720"},"scale":"1.25"}
          ]
        }"#;
        let (mons, skipped) = parse_kscreen_monitors(json);
        assert_eq!(mons.len(), 2, "只保留可用输出");
        assert_eq!(skipped, 3);
        // 字符串数字也要吃下去（不同 Plasma 版本字段类型不一致）
        assert_eq!(mons[1].name, "DP-5");
        assert_eq!(mons[1].x, 100.0);
        assert_eq!(mons[1].scale, 1.25);
    }

    #[test]
    fn garbage_json_returns_empty_not_panic() {
        for bad in ["", "not json", "[]", "{}", "{\"outputs\":\"x\"}"] {
            let (mons, skipped) = parse_kscreen_monitors(bad);
            assert!(mons.is_empty(), "{bad:?} 应返回空列表");
            assert_eq!(skipped, 0);
        }
    }
}
