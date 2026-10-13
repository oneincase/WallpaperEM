//! KDE/Linux 托盘图标与开机自启的两处「平台策略」收口点。
//!
//! 为什么单独成文件：本模块在 `target_os = "linux"` **或** 测试构建下编译（见
//! [`crate`] 里 `mod tray_linux;` 的 cfg）。开发机是 macOS，`#[cfg(target_os = "linux")]`
//! 的调用点不参与类型检查，所以把能移动的逻辑全放在这里：即使宿主不是 Linux，
//! `cargo test --lib` 也会真实编译并执行下面这些纯函数；真正碰 tauri/文件系统的薄封装
//! （[`apply_tray_icon`]、[`ensure_autostart_entry_keys`]）只在 Linux 构建里被调用。
//!
//! ## 1. 托盘图标
//!
//! `icons/tray-44.png` 是给 macOS `icon_as_template(true)` 用的纯黑剪影：实测 44×44、
//! 1552 个不透明像素、不透明像素亮度极差 = 0。Linux 托盘主机（KDE Plasma 的
//! StatusNotifierItem 宿主、AppIndicator）不做 symbolic 重着色，原色直出 ——
//! Plasma 默认深色面板上纯黑剪影 = 托盘里「看不见图标」。
//!
//! 旧代码在非 macOS 上直接用 `default_window_icon()`；现在 Linux 先判断内嵌图的对比度，
//! 不可用就退回彩色应用图标（tauri 在 Unix 上取 `bundle.icon` 里第一个 `.png`，
//! 见 `tauri-codegen-2.6.3/src/context.rs:234-242` + `src/context.rs:495-509` 的 `find_icon`，
//! 本项目即 `icons/32x32.png`：亮度极差 222、含 348 个彩色像素 —— 实测可用）。
//!
//! ## 2. 开机自启
//!
//! `tauri-plugin-autostart` 在 Linux 上落到 `auto-launch` 0.5.0，它写出的
//! `~/.config/autostart/{app_name}.desktop` 只有基础键（模板见本机
//! `$CARGO_HOME/registry/src/index.crates.io-1949cf8c6b5b557f/auto-launch-0.5.0/src/linux.rs:32-52`），
//! 没有 `X-GNOME-Autostart-enabled`。插件没有「自定义 .desktop 内容」的接口
//! （`AutoLaunchBuilder` 只提供 `set_app_name` / `set_app_path` / `set_args` /
//! `set_use_launch_agent`，见 auto-launch-0.5.0/src/lib.rs:274-320），
//! 所以只能启动后**幂等补键**。
//!
//! 补键只动 `[Desktop Entry]` 组，已有键不覆盖，文件不存在（用户没开自启）不创建 ——
//! 用户自己的 `.desktop` 内容不会被重写。

use std::path::{Path, PathBuf};

/// Linux 托盘图标资源（与 lib.rs macOS 分支用的是同一个文件）。
pub(crate) const TRAY_ASSET: &str = "icons/tray-44.png";

/// 亮度极差阈值（8bit）：低于此认为「不透明像素几乎同色」= 单色剪影，
/// 只能靠托盘主机的 symbolic 重着色才看得见。
const SPREAD_MIN: i32 = 32;

/// alpha 低于此的像素不计入统计（这类像素本来就是背景）。
const ALPHA_MIN: u8 = 8;

/// 一张 RGBA 位图能否在「原色直出」（不做 symbolic 重着色）的托盘上直接用。
///
/// 两类不可用：
/// - 没有任何不透明像素（空图 / 全透明）；
/// - 不透明像素的亮度极差过小（纯黑剪影、纯白剪影）—— 这类图是给 macOS
///   `icon_as_template(true)` 用的，系统按菜单栏深浅重新着色；托盘主机若原色直出，
///   深色面板上纯黑、浅色面板上纯白都等于看不见。
///
/// 实测：`icons/tray-44.png` 极差 0 → false；`icons/32x32.png` 极差 222 → true
/// （回归测试 `tray_real_assets_are_classified_correctly`）。
pub(crate) fn has_usable_contrast(rgba: &[u8]) -> bool {
    let mut min = i32::MAX;
    let mut max = i32::MIN;
    for px in rgba.chunks_exact(4) {
        if px[3] < ALPHA_MIN {
            continue;
        }
        let luma = (299 * px[0] as i32 + 587 * px[1] as i32 + 114 * px[2] as i32) / 1000;
        min = min.min(luma);
        max = max.max(luma);
    }
    // 一个不透明像素都没有时 min = i32::MAX、max = i32::MIN，这里为 false；
    // `&&` 短路也保证 (max - min) 只在两者都落在 [0, 255] 时计算，不会溢出。
    min <= max && (max - min) >= SPREAD_MIN
}

/// Linux 托盘图标来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayIconSource {
    /// 内嵌的 `icons/tray-44.png`：自身对比度足够，能原色直出。
    Embedded,
    /// 应用默认窗口图标：tauri 在 Unix 上挑的 `bundle.icon` 首个 png，即彩色应用 logo。
    AppIcon,
    /// 两者都没有：建托盘但不带图标，只记日志。
    None,
}

/// 选择 Linux 托盘图标：内嵌图对比度不足时退回彩色应用图标。
pub(crate) fn choose_tray_icon(embedded_usable: bool, has_app_icon: bool) -> TrayIconSource {
    if embedded_usable {
        TrayIconSource::Embedded
    } else if has_app_icon {
        TrayIconSource::AppIcon
    } else {
        TrayIconSource::None
    }
}

/// Linux 托盘图标落地：按 [`choose_tray_icon`] 的决策设置图标。
///
/// 永不设置 `icon_as_template`（Linux 托盘主机不重着色）；任何失败路径都只记日志、
/// 不 panic —— 托盘图标缺失不该让应用启动失败。
pub(crate) fn apply_tray_icon<R: tauri::Runtime>(
    mut builder: tauri::tray::TrayIconBuilder<R>,
    app: &tauri::AppHandle<R>,
) -> tauri::tray::TrayIconBuilder<R> {
    let embedded = tauri::include_image!("icons/tray-44.png");
    let embedded_usable = has_usable_contrast(embedded.rgba());
    let app_icon = app.default_window_icon().cloned();
    match (choose_tray_icon(embedded_usable, app_icon.is_some()), app_icon) {
        (TrayIconSource::Embedded, _) => {
            tracing::info!(
                "tray icon(linux): {} {}x{} 自身对比度足够，原色直出",
                TRAY_ASSET,
                embedded.width(),
                embedded.height()
            );
            builder = builder.icon(embedded);
        }
        (TrayIconSource::AppIcon, Some(icon)) => {
            tracing::info!(
                "tray icon(linux): {} 是单色剪影（对比度不足），退回 default_window_icon {}x{}",
                TRAY_ASSET,
                icon.width(),
                icon.height()
            );
            builder = builder.icon(icon);
        }
        // choose_tray_icon 已经保证不会走到这里；兜底只是为了不 panic。
        (TrayIconSource::AppIcon, None) => {
            tracing::warn!("tray icon(linux): default_window_icon 判定为有、取值却为 None，托盘不带图标");
        }
        (TrayIconSource::None, _) => {
            tracing::warn!(
                "tray icon(linux): {} 对比度不足且 default_window_icon 为空，托盘将不带图标",
                TRAY_ASSET
            );
        }
    }
    builder
}

/// autostart `.desktop` 需要补的键（写进 `[Desktop Entry]` 组）。
///
/// - `X-GNOME-Autostart-enabled=true`：GNOME 会话读的「启用」键。
///   来源：本任务 task-5 的明确要求。**未核实 KDE 是否读取它** —— 本机 `web_fetch`
///   不可用（域名解析到非公网地址），查不到权威原文，所以它只当**幂等冗余键**用，
///   不承担 KDE 语义。
/// - `StartupNotify=false`：auto-launch 现在的模板里已经有这一条
///   （auto-launch-0.5.0/src/linux.rs:32-52），列在这里是为了「模板变了也不会丢」，
///   正常情况下它是空操作。
///
/// 未采纳的候选键（**证据不足，故意不写进代码**）：`X-KDE-autostart-after`、
/// `X-KDE-autostart-condition`、`X-KDE-StartupNotify` 等 KDE 专有键只知道名字，
/// 查不到 KDE 源码原文，写错反而可能让 KDE 忽略条目或延迟启动。
pub(crate) const AUTOSTART_KEYS: &[(&str, &str)] = &[
    ("X-GNOME-Autostart-enabled", "true"),
    ("StartupNotify", "false"),
];

/// 一次补键的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutostartPatch {
    /// 补键后的完整文件内容（无改动时 = 原内容）。
    pub(crate) content: String,
    /// 本次实际插入的键名（幂等：再调一次为空）。
    pub(crate) added: Vec<&'static str>,
}

/// 在 `.desktop` 内容里幂等补 [`AUTOSTART_KEYS`]。
///
/// 规则：只改 `[Desktop Entry]` 组；同名键只要文件里任何一组已存在就不重复写
/// （避免写坏成重复键）；找不到 `[Desktop Entry]` 组就原样返回。CRLF 与结尾换行保持原样。
pub(crate) fn patch_desktop_entry(content: &str) -> AutostartPatch {
    let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let ends_with_eol = content.ends_with('\n');
    let mut lines: Vec<String> = content
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();
    if ends_with_eol {
        // split 在末尾会多出一个空串，去掉它，最后按 ends_with_eol 补回
        lines.pop();
    }

    let Some(header) = lines.iter().position(|l| l.trim() == "[Desktop Entry]") else {
        return AutostartPatch {
            content: content.to_string(),
            added: Vec::new(),
        };
    };
    // 组结束 = 下一个组头（或文件尾）
    let group_end = lines
        .iter()
        .enumerate()
        .skip(header + 1)
        .find(|(_, l)| l.trim_start().starts_with('['))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());

    // 存在性只看含 '=' 的行的 key 部分，且不限组（别的组有同名键也算有）
    let has_key = |key: &str| {
        lines
            .iter()
            .any(|l| l.split_once('=').is_some_and(|(k, _)| k.trim() == key))
    };

    let missing: Vec<(&'static str, &'static str)> = AUTOSTART_KEYS
        .iter()
        .copied()
        .filter(|(k, _)| !has_key(k))
        .collect();
    if missing.is_empty() {
        return AutostartPatch {
            content: content.to_string(),
            added: Vec::new(),
        };
    }

    // 插在组内最后一个非空行之后，键留在 [Desktop Entry] 组里
    let mut at = group_end;
    while at > header + 1 && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    for (i, (key, value)) in missing.iter().enumerate() {
        lines.insert(at + i, format!("{key}={value}"));
    }

    let mut out = lines.join(eol);
    if ends_with_eol {
        out.push_str(eol);
    }
    AutostartPatch {
        content: out,
        added: missing.iter().map(|(k, _)| *k).collect(),
    }
}

/// autostart 目录：与 auto-launch 0.5.0 保持一致（`dirs::home_dir()/.config/autostart`，
/// 见 auto-launch-0.5.0/src/linux.rs:81-82）。插件那里是 `.unwrap()`，这里返回 `Option`
/// 以免 home 取不到时 panic。
pub(crate) fn autostart_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".config").join("autostart"))
}

/// `~/.config/autostart/{app_name}.desktop`：文件名规则同 auto-launch-0.5.0/src/linux.rs:75-76。
///
/// `app_name` 由 `tauri-plugin-autostart` 2.5.1 取 `app.package_info().name`
/// （tauri-plugin-autostart-2.5.1/src/lib.rs:178-182）；调用方用同一来源，
/// 保证补键的就是插件写出/读取的那个文件。
pub(crate) fn autostart_desktop_path(app_name: &str) -> Option<PathBuf> {
    Some(autostart_dir()?.join(format!("{app_name}.desktop")))
}

/// 就地给一个 autostart `.desktop` 补键（先读、内容不变就不写文件）。
///
/// 返回 `Ok(None)` = 键已齐全、无需改动；`Ok(Some(keys))` = 已写入并返回补的键。
pub(crate) fn patch_autostart_file(path: &Path) -> std::io::Result<Option<Vec<&'static str>>> {
    let content = std::fs::read_to_string(path)?;
    let patched = patch_desktop_entry(&content);
    if patched.added.is_empty() {
        return Ok(None);
    }
    std::fs::write(path, patched.content)?;
    Ok(Some(patched.added))
}

/// 启动时调用（Linux）：文件不存在（用户没开自启 / 还没写过）就什么都不做；
/// 任何失败只 warn —— 自启条目补键不该影响 setup。
pub(crate) fn ensure_autostart_entry_keys<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let app_name = app.package_info().name.clone();
    let Some(path) = autostart_desktop_path(&app_name) else {
        tracing::warn!("autostart: 取不到 home 目录，跳过 .desktop 补键");
        return;
    };
    if !path.exists() {
        tracing::debug!("autostart: {} 不存在（未开启自启），跳过补键", path.display());
        return;
    }
    match patch_autostart_file(&path) {
        Ok(None) => tracing::debug!("autostart: {} 键已齐全，无需改动", path.display()),
        Ok(Some(keys)) => tracing::info!("autostart: {} 补写键 {:?}", path.display(), keys),
        Err(e) => tracing::warn!("autostart: 补写 {} 失败: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 类型形状见证：Linux 上的两处调用点被 `#[cfg(target_os = "linux")]` 摘掉了，
    /// 在 macOS 宿主上不会被类型检查；这里用同一组类型把它们再拼一遍 ——
    /// 参数/返回值形状写错（泛型、借用）的话这个函数就编不过（永远不会被调用）。
    #[allow(dead_code)]
    fn call_site_shape_witness<R: tauri::Runtime>(
        builder: tauri::tray::TrayIconBuilder<R>,
        app: &tauri::AppHandle<R>,
    ) -> tauri::tray::TrayIconBuilder<R> {
        ensure_autostart_entry_keys(app);
        apply_tray_icon(builder, app)
    }

    /// auto-launch 0.5.0 现在写出的模板（auto-launch-0.5.0/src/linux.rs:32-52），
    /// 逐字抄来当夹具：注意 `Exec` 后面那个空格是插件 `Exec={} {}` 拼出来的。
    const AUTO_LAUNCH_TEMPLATE: &str = "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name=WallpaperEM\n\
         Comment=WallpaperEM startup script\n\
         Exec=/usr/bin/WallpaperEM \n\
         StartupNotify=false\n\
         Terminal=false";

    fn px(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
        [r, g, b, a]
    }

    // ---- 托盘图标 ----

    #[test]
    fn tray_has_usable_contrast_rejects_all_transparent() {
        let buf: Vec<u8> = (0..16).flat_map(|_| px(0, 0, 0, 0)).collect();
        assert!(!has_usable_contrast(&buf), "全透明 = 什么都看不见");
    }

    #[test]
    fn tray_has_usable_contrast_rejects_flat_silhouette() {
        let black: Vec<u8> = (0..16).flat_map(|_| px(0, 0, 0, 255)).collect();
        assert!(!has_usable_contrast(&black), "纯黑剪影在深色面板上不可见");
        let white: Vec<u8> = (0..16).flat_map(|_| px(255, 255, 255, 255)).collect();
        assert!(!has_usable_contrast(&white), "纯白剪影在浅色面板上不可见");
    }

    #[test]
    fn tray_has_usable_contrast_accepts_mixed_luma_and_ignores_alpha() {
        let mixed = [px(0, 0, 0, 255), px(255, 255, 255, 255)]
            .concat();
        assert!(has_usable_contrast(&mixed), "黑白混排 = 有对比度");
        // 近透明像素只当背景：白色不透明像素缺失时仍是纯黑剪影
        let white_transparent = [px(0, 0, 0, 255), px(255, 255, 255, 0)].concat();
        assert!(!has_usable_contrast(&white_transparent));
    }

    #[test]
    fn tray_real_assets_are_classified_correctly() {
        // 编译期解码（tauri::include_image!），不需要图像库
        let tray = tauri::include_image!("icons/tray-44.png");
        assert_eq!((tray.width(), tray.height()), (44, 44));
        assert_eq!(
            has_usable_contrast(tray.rgba()),
            false,
            "icons/tray-44.png 是纯黑剪影：Linux 托盘原色直出时必须判为不可用"
        );
        let app_icon = tauri::include_image!("icons/32x32.png");
        assert_eq!((app_icon.width(), app_icon.height()), (32, 32));
        assert!(
            has_usable_contrast(app_icon.rgba()),
            "icons/32x32.png 是彩色应用图标，必须判为可用"
        );
    }

    #[test]
    fn tray_icon_choice_prefers_embedded_then_app_icon() {
        assert_eq!(choose_tray_icon(true, true), TrayIconSource::Embedded);
        assert_eq!(choose_tray_icon(true, false), TrayIconSource::Embedded);
        assert_eq!(choose_tray_icon(false, true), TrayIconSource::AppIcon);
        assert_eq!(choose_tray_icon(false, false), TrayIconSource::None);
    }

    // ---- 开机自启 ----

    #[test]
    fn autostart_patch_fills_plugin_template() {
        let patched = patch_desktop_entry(AUTO_LAUNCH_TEMPLATE);
        assert_eq!(patched.added, vec!["X-GNOME-Autostart-enabled"]);
        // 夹具没有结尾换行，补的键直接跟在本组最后一行后面
        assert!(
            patched.content.ends_with("X-GNOME-Autostart-enabled=true"),
            "补的键要落在 [Desktop Entry] 组里：{}",
            patched.content
        );
        // 逐字契约：补键后的完整内容（供评审核对，不再靠肉眼看）
        assert_eq!(
            patched.content,
            "[Desktop Entry]\nType=Application\nVersion=1.0\nName=WallpaperEM\n\
             Comment=WallpaperEM startup script\nExec=/usr/bin/WallpaperEM \n\
             StartupNotify=false\nTerminal=false\nX-GNOME-Autostart-enabled=true"
        );
        assert_eq!(
            patched.content.matches("StartupNotify=").count(),
            1,
            "模板里已有的 StartupNotify 不能写重"
        );
        assert!(patched.content.starts_with("[Desktop Entry]\n"));
    }

    #[test]
    fn autostart_patch_is_idempotent() {
        let once = patch_desktop_entry(AUTO_LAUNCH_TEMPLATE);
        let twice = patch_desktop_entry(&once.content);
        assert!(twice.added.is_empty(), "第二次必须无改动");
        assert_eq!(twice.content, once.content, "内容必须逐字不变");
    }

    #[test]
    fn autostart_patch_never_overwrites_existing_value() {
        // 只有 X-GNOME（值为 false）时：只补缺的 StartupNotify，绝不把 false 改成 true
        let partial = "[Desktop Entry]\nName=X\nX-GNOME-Autostart-enabled=false\n";
        let patched = patch_desktop_entry(partial);
        assert_eq!(patched.added, vec!["StartupNotify"]);
        assert!(patched.content.contains("X-GNOME-Autostart-enabled=false"));
        assert!(
            !patched.content.contains("X-GNOME-Autostart-enabled=true"),
            "外部写过的值不能被覆盖"
        );

        // 两个键都在 → 一个字节都不改
        let full = "[Desktop Entry]\nName=X\nX-GNOME-Autostart-enabled=false\nStartupNotify=false\n";
        let untouched = patch_desktop_entry(full);
        assert!(untouched.added.is_empty(), "{:?}", untouched.added);
        assert_eq!(untouched.content, full);
    }

    #[test]
    fn autostart_patch_keeps_other_groups_and_blank_lines() {
        let content = "[Desktop Entry]\nName=X\n\n[Desktop Action Foo]\nName=Bar\n";
        let patched = patch_desktop_entry(content);
        let head = patched.content.find("X-GNOME-Autostart-enabled").unwrap();
        let other = patched.content.find("[Desktop Action Foo]").unwrap();
        assert!(head < other, "补的键要留在第一个组里");
        assert!(
            patched.content.contains("\n\n[Desktop Action Foo]\nName=Bar\n"),
            "其他组与空行原样保留：{}",
            patched.content
        );
    }

    #[test]
    fn autostart_patch_preserves_crlf_and_missing_group() {
        let crlf = "[Desktop Entry]\r\nName=X\r\n";
        let patched = patch_desktop_entry(crlf);
        assert!(
            patched.content.contains("Name=X\r\nX-GNOME-Autostart-enabled=true\r\n"),
            "CRLF 文件保持 CRLF：{:?}",
            patched.content
        );
        // 没有 [Desktop Entry] 组：原样返回，不猜、不改
        let no_group = "Name=X\n";
        let untouched = patch_desktop_entry(no_group);
        assert!(untouched.added.is_empty());
        assert_eq!(untouched.content, no_group);
    }

    #[test]
    fn autostart_desktop_path_matches_plugin_layout() {
        let path = autostart_desktop_path("WallpaperEM").expect("macOS/Linux 上 home_dir 应该可用");
        assert!(
            path.ends_with(".config/autostart/WallpaperEM.desktop"),
            "路径要与 auto-launch 一致：{}",
            path.display()
        );
        assert_eq!(autostart_dir().unwrap(), path.parent().unwrap());
    }

    #[test]
    fn autostart_patch_file_roundtrip_writes_only_when_needed() {
        let dir = std::env::temp_dir().join(format!(
            "wallpaperem-autostart-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let file = dir.join("WallpaperEM.desktop");

        std::fs::write(&file, AUTO_LAUNCH_TEMPLATE).expect("写夹具");
        let added = patch_autostart_file(&file).expect("补键");
        assert_eq!(added.as_deref(), Some(&["X-GNOME-Autostart-enabled"][..]));
        let text = std::fs::read_to_string(&file).expect("回读");
        assert!(text.contains("X-GNOME-Autostart-enabled=true"));

        // 第二次：键已齐全 → 不写文件
        assert_eq!(patch_autostart_file(&file).expect("二次补键"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
