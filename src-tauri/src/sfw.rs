//! SFW（工作友好）过滤：统一判定「成人 / NSFW」内容，供工坊、本地库、收藏共用。
//!
//! Steam 创意工坊用官方标签 `Mature` 标注成人内容（R-Content / 成人向），
//! 此外再叠加标题 / 描述 / 作者 / 标签关键词兜底。

use rusqlite::Connection;

use crate::db;

/// 成人 / NSFW 标签（小写，命中即视为成人）。
/// `Mature` 是 Steam 工坊官方成人标签；其余为常见 NSFW / R18 标签做兜底。
pub fn is_adult_tag(tag: &str) -> bool {
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "mature" | "nsfw" | "r18" | "adult" | "adult content" | "porn" | "hentai" | "lewd"
    )
}

/// 标签列表是否含成人标签
pub fn is_adult_tags(tags: &[String]) -> bool {
    tags.iter().any(|t| is_adult_tag(t))
}

/// 文本（标题 / 描述 / 作者）是否含成人关键词
pub fn is_adult_text(text: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "nsfw", "porn", "hentai", "x-rated", "r18", "18+", "lewd", "nude", "naked",
    ];
    let t = text.to_lowercase();
    KEYWORDS.iter().any(|k| t.contains(k))
}

/// 工作友好开关是否开启（默认开启）
pub fn is_family_friendly(conn: &Connection) -> bool {
    db::get_setting(conn, "family_friendly")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true)
}

/// 工作友好模式下，某条目（标题 + 标签）是否应被过滤
pub fn is_adult_item(title: &str, tags: &[String]) -> bool {
    is_adult_text(title) || is_adult_tags(tags)
}

/// 工作友好模式下，完整条目（标题 / 描述 / 作者 / 标签）是否应被过滤
pub fn is_adult_full(title: &str, description: &str, creator: Option<&str>, tags: &[String]) -> bool {
    is_adult_text(title)
        || is_adult_text(description)
        || creator.map(is_adult_text).unwrap_or(false)
        || is_adult_tags(tags)
}
