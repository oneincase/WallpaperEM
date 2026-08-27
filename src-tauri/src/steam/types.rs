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
    /// WallpaperType 或空串（不过滤）
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub page: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkshopSearchResult {
    pub items: Vec<WorkshopItemSummary>,
    pub total: usize,
    pub page: u32,
    pub page_size: usize,
    pub has_more: bool,
}
