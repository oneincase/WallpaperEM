//! 工坊条目详情批量接口（对齐 Web 版 providers/details.ts）
//!
//! POST ISteamRemoteStorage/GetPublishedFileDetails/v1（表单，一次 ≤30 个 id）
//! 返回条目完整元数据；类型从标签推断。

use super::SteamClient;
use super::types::WorkshopItem;
use serde::{Deserialize, Deserializer};

pub const DETAILS_URL: &str =
    "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/";

/// 容错反序列化：Steam 对 file_size 等字段返回字符串（"32227764"），
/// 其他字段可能是数字；统一兼容 string|number|null。
fn de_i64<'de, D>(d: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let v: Option<serde_json::Value> = Option::deserialize(d)?;
    Ok(v.and_then(|v| match v {
        serde_json::Value::String(s) => s.parse().ok(),
        serde_json::Value::Number(n) => n.as_i64(),
        _ => None,
    }))
}

#[derive(Deserialize)]
struct DetailsResp {
    response: Option<DetailsBody>,
}

#[derive(Deserialize)]
struct DetailsBody {
    publishedfiledetails: Option<Vec<SteamDetail>>,
}

#[derive(Deserialize)]
struct SteamDetail {
    result: Option<i64>,
    publishedfileid: Option<serde_json::Value>,
    title: Option<String>,
    description: Option<String>,
    preview_url: Option<String>,
    file_url: Option<String>,
    #[serde(default, deserialize_with = "de_i64")]
    file_size: Option<i64>,
    creator: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "de_i64")]
    subscriptions: Option<i64>,
    #[serde(default, deserialize_with = "de_i64")]
    favorited: Option<i64>,
    #[serde(default, deserialize_with = "de_i64")]
    time_created: Option<i64>,
    #[serde(default, deserialize_with = "de_i64")]
    time_updated: Option<i64>,
    tags: Option<Vec<TagItem>>,
}

#[derive(Deserialize)]
struct TagItem {
    tag: Option<String>,
}

/// 从标签/类型名推断壁纸类型（兼容大写标签 "Video" 与 project.json 小写 "video"）
pub fn infer_type_from_tags(tags: &[String]) -> String {
    for t in tags {
        match t.to_ascii_lowercase().as_str() {
            "video" => return "video".into(),
            "scene" => return "scene".into(),
            "web" => return "web".into(),
            "gif" => return "gif".into(),
            "application" => return "application".into(),
            _ => {}
        }
    }
    "unknown".into()
}

/// 批量获取条目完整元数据（一次 ≤30 个；结果仅含 result==1 的有效条目）
pub async fn get_item_details(client: &SteamClient, ids: &[String]) -> Result<Vec<WorkshopItem>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut fields: Vec<(String, String)> =
        vec![("itemcount".into(), ids.len().to_string())];
    for (i, id) in ids.iter().enumerate() {
        fields.push((format!("publishedfileids[{i}]"), id.clone()));
    }
    let refs: Vec<(&str, &str)> = fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let resp = client.post_form(DETAILS_URL, &refs).await?;
    let data: DetailsResp = resp.json().await.map_err(|e| format!("详情接口解析失败: {e}"))?;

    let mut out = Vec::new();
    for d in data.response.and_then(|r| r.publishedfiledetails).unwrap_or_default() {
        if d.result != Some(1) {
            continue;
        }
        let Some(id) = d.publishedfileid else { continue };
        let tags: Vec<String> = d
            .tags
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| t.tag)
            .collect();
        out.push(WorkshopItem {
            id: json_to_string(&id),
            title: d.title.unwrap_or_else(|| "未命名".into()),
            description: d.description.unwrap_or_default(),
            preview_url: d.preview_url.unwrap_or_default(),
            file_url: d.file_url,
            file_size: d.file_size,
            creator: d.creator.as_ref().map(json_to_string),
            tags: tags.clone(),
            r#type: infer_type_from_tags(&tags),
            subscriptions: d.subscriptions,
            favorited: d.favorited,
            time_created: d.time_created,
            time_updated: d.time_updated,
        });
    }
    Ok(out)
}

fn json_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => v.to_string(),
    }
}
