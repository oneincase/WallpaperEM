//! 后端 i18n：托盘菜单 / 原生窗口标题 / 文件选择框这些**不经过前端**的文案。
//!
//! 界面文案的主战场是 src/lib/i18n.ts（那边有「中文原文当键」的取舍说明）。
//! 后端只留这一小撮系统级文案，沿用同一套约定：中文原文当键、查不到就原样返回。
//! 好处是漏翻只会露出中文，不会露出 key 或空白。
//!
//! 语言的权威在前端：用户在设置里选了什么，由 `app_set_locale` 推过来。
//! 启动默认值只是兜底 —— 主窗口的 JS 起来（几百毫秒）就会推一次，
//! 兜底准不准只影响这一小段窗口期内托盘菜单长什么样。

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Locale {
    ZhCn,
    EnUs,
}

/// 当前语言。0 = zh-CN（默认，中文环境下 tr() 零开销短路），1 = en-US。
/// 用原子量而不是 OnceLock：用户随时可以来回切，必须能反复写。
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn locale() -> Locale {
    if CURRENT.load(Ordering::Relaxed) == 1 {
        Locale::EnUs
    } else {
        Locale::ZhCn
    }
}

pub fn is_en() -> bool {
    locale() == Locale::EnUs
}

/// 前端推来的语言标签（`"zh-CN"` / `"en-US"`）。只区分「是不是英文」——
/// 将来加语言时这里扩展，前端那张表也要跟着加（见 i18n.ts 的 LOCALES）。
pub fn set_locale(tag: &str) {
    let en = tag.to_ascii_lowercase().starts_with("en");
    CURRENT.store(u8::from(en), Ordering::Relaxed);
}

/// 启动兜底：跟随系统语言。只读环境变量 —— GUI 启动的 macOS/Windows 进程
/// 拿不到 LANG，那里会落到中文默认值，等前端推过来即修正（见模块头）。
pub fn init_from_system() {
    let tag = ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    // 判定规则必须与前端 detectSystemLocale()（src/lib/i18n.ts）一致，
    // 否则「前端推之前」和「推之后」会给出不同答案，表现为托盘闪一下再变
    let lower = tag.to_ascii_lowercase();
    let en = ["en", "de", "fr", "es", "pt", "it", "nl", "ru", "ja", "ko"]
        .iter()
        .any(|p| lower.starts_with(p));
    CURRENT.store(u8::from(en), Ordering::Relaxed);
}

/// 取词：中文原文 → 当前语言。查不到就原样返回中文（宁可露出中文，也不要空白）。
pub fn tr<'a>(zh: &'a str) -> &'a str {
    if !is_en() {
        return zh;
    }
    en(zh).unwrap_or(zh)
}

/// 后端文案表：中文原文 → 英文。用 match 而不是 HashMap —— 零分配、返回
/// `&'static str`，且重复的键会被编译器当成 unreachable pattern 警告出来。
fn en(zh: &str) -> Option<&'static str> {
    Some(match zh {
        // ---- 托盘菜单 ----
        "显示主窗口" => "Show Main Window",
        "壁纸设置" => "Wallpaper Settings",
        "自动暂停" => "Auto Pause",
        // 与设置页 / 独立设置窗口的同名行一致（src/locales/en-US.ts 的「显示模式」）
        "显示模式" => "Display mode",
        "裁剪" => "Crop",
        "缩放" => "Fit",
        "拉伸" => "Stretch",
        "清晰度" => "Quality",
        "省电" => "Battery Saver",
        "标准" => "Standard",
        "高清" => "High",
        "帧率上限" => "FPS Limit",
        "滤镜效果" => "Filter",
        "退出" => "Quit",
        // ---- macOS 应用菜单 ----
        "最小化主窗口" => "Minimize Main Window",
        // ---- 原生文件选择框 ----
        "壁纸" => "Wallpapers",
        "文件" => "Files",
        // ---- Steam 接口的「方法名」：会被拼进错误句子里（见 steam/auth.rs），
        //      所以必须在**拼接之前**取词，拼完就没法单独翻了 ----
        "登录" => "Sign-in",
        "扫码登录" => "QR sign-in",
        "验证码提交" => "Code submission",
        "轮询登录状态" => "Sign-in status polling",
        // ---- 滤镜（与 wallpaper::WALLPAPER_FILTERS 的标签一一对应）----
        "无" => "None",
        "高斯模糊" => "Gaussian Blur",
        "黑白" => "Monochrome",
        "怀旧" => "Sepia",
        "鲜艳" => "Vivid",
        "暖色" => "Warm",
        "冷色" => "Cool",
        "反色" => "Invert",
        "提亮" => "Brighten",
        "压暗" => "Darken",
        "高对比" => "High Contrast",
        _ => return None,
    })
}

/// 前端在设置里切语言时调用：落定语言，并把**原生绘制**的文案重写一遍。
/// 不返回值：前端那边是 fire-and-forget（切语言不该因为托盘不可用而报错）。
#[tauri::command]
pub fn app_set_locale(app: tauri::AppHandle, locale: String) {
    set_locale(&locale);
    crate::retranslate_native_ui(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en_table_hits_and_falls_back() {
        assert_eq!(en("显示主窗口"), Some("Show Main Window"));
        assert_eq!(en("没有这条"), None);
    }

    #[test]
    fn tr_follows_locale_and_falls_back_to_chinese() {
        // 本模块的测试串行跑（其它测试不碰语言），切完记得还原，避免影响同进程测试
        set_locale("en-US");
        assert_eq!(tr("壁纸设置"), "Wallpaper Settings");
        assert_eq!(tr("这句没翻译"), "这句没翻译");
        set_locale("zh-CN");
        assert_eq!(tr("壁纸设置"), "壁纸设置");
        assert!(!is_en());
    }

    #[test]
    fn locale_tag_parsing() {
        set_locale("en_US.UTF-8");
        assert!(is_en());
        set_locale("zh-Hans-CN");
        assert!(!is_en());
    }

    #[test]
    fn system_tag_detection_matches_frontend() {
        // 与 src/lib/i18n.ts 的 detectSystemLocale() 同一张表
        for (tag, want_en) in [
            ("en_US.UTF-8", true),
            ("ja_JP.UTF-8", true),
            ("de_DE", true),
            ("zh_CN.UTF-8", false),
            ("zh-Hant-TW", false),
            ("", false),
        ] {
            let lower = tag.to_ascii_lowercase();
            let en = ["en", "de", "fr", "es", "pt", "it", "nl", "ru", "ja", "ko"]
                .iter()
                .any(|p| lower.starts_with(p));
            assert_eq!(en, want_en, "tag={tag}");
        }
    }
}
