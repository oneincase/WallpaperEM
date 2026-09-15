//! 工坊共享类型（与 packages/shared 对齐，serde camelCase ↔ 前端 TS）

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkshopItem {
    pub id: String,
    pub title: String,
    pub description: String,
    pub preview_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    pub tags: Vec<String>,
    /// video|scene|web|gif|application|unknown
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscriptions: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favorited: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_created: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_updated: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkshopItemSummary {
    pub id: String,
    pub title: String,
    pub preview_url: String,
    pub tags: Vec<String>,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscriptions: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favorited: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_created: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkshopSearchParams {
    #[serde(default)]
    pub query: Option<String>,
    /// WallpaperType 或空串（不过滤）。与 tags 里的 Scene/Video/Web 等价，
    /// 保留是为了兼容旧调用方与类型按钮组。
    #[serde(default)]
    pub r#type: Option<String>,
    /// 分组标签：组内并集（OR）、组间交集（AND），空 = 不约束。
    /// 优先于 tags；Steam 原生只支持严格 AND，多标签组由后端拆多查询合并
    #[serde(default)]
    pub tag_groups: Vec<Vec<String>>,
    /// 旧版平面必需标签（Steam 侧严格 AND），等价于每个标签自成一组
    #[serde(default)]
    pub tags: Vec<String>,
    /// 排除标签
    #[serde(default)]
    pub excluded_tags: Vec<String>,
    #[serde(default)]
    pub sort: Option<String>,
    /// 趋势时间范围（天）。仅 sort=trend 时生效
    #[serde(default)]
    pub days: Option<u32>,
    #[serde(default)]
    pub created_after: Option<i64>,
    #[serde(default)]
    pub created_before: Option<i64>,
    #[serde(default)]
    pub updated_after: Option<i64>,
    #[serde(default)]
    pub updated_before: Option<i64>,
    #[serde(default)]
    pub page: Option<u32>,
}

/// 作者名片：Steam 个人资料解析结果（昵称 + 头像）。
/// project.json 本身没有 author 字段，作者 = 工坊条目 creator(SteamID64)
/// 经 steam/profile.rs 抓取解析而来。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorSummary {
    pub steam_id: String,
    pub name: String,
    pub avatar_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkshopSearchResult {
    pub items: Vec<WorkshopItemSummary>,
    pub total: usize,
    pub page: u32,
    pub page_size: usize,
    pub has_more: bool,
    /// 标签组合数超出后端上限、只查了一部分组合：结果不完整，UI 要提示用户
    /// 少选几个标签。带 `serde(default)` 是为了兼容旧版前端快照/cache 反序列化。
    #[serde(default)]
    pub truncated: bool,
}
