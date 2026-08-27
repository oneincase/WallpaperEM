//! 工坊浏览页抓取（SSR HTML 解析，对齐 Web 版 providers/browse.ts）
//!
//! Steam 2025 改版后 format=json 已移除，新 UI 条目卡片服务端渲染在 HTML 中：
//! - 锚点 `sharedfiles/filedetails/?id=<id>` 抓 id/预览图/标题（锚点比数据块稳定）
//! - 总数从内嵌 SSR 状态提取：`total_count\" : <n>`（多层转义，宽松正则）

use std::collections::{HashMap, HashSet};
use super::SteamClient;
use crate::util::{decode_entities, url_encode};

pub const BROWSE_URL: &str = "https://steamcommunity.com/workshop/browse/";

#[derive(Default)]
pub struct BrowseQuery {
    pub query: Option<String>,
    pub sort: Option<String>,
    pub page: Option<u32>,
    /// 类型/标签过滤（AND）：如 ["Video", "1080p"]
    pub required_tags: Vec<String>,
}

pub struct BrowseRawItem {
    pub id: String,
    pub title: String,
    pub preview_url: String,
}

pub struct BrowseRawResult {
    pub items: Vec<BrowseRawItem>,
    /// 匹配结果总数（内嵌 SSR 状态 total_count，0 = 未解析到）
    pub total: usize,
    pub has_more: bool,
}

pub async fn browse_workshop_raw(
    client: &SteamClient,
    q: &BrowseQuery,
) -> Result<BrowseRawResult, String> {
    let sort = q.sort.as_deref().unwrap_or("trend");
    // Steam 当前 SSR 页用 `browsesort` 控制排序；`actualsort` 会被忽略（实测总是返回 trend 序）。
    let mut url = format!(
        "{BROWSE_URL}?appid=431960&section=readytouseitems&browsesort={sort}&actualsort={sort}&p={}",
        q.page.unwrap_or(1)
    );
    if let Some(query) = &q.query {
        url += &format!("&searchtext={}", url_encode(query));
    }
    for t in &q.required_tags {
        url += &format!("&requiredtags[]={}", url_encode(t));
    }

    let resp = client.get(&url).await?;
    let html = resp.text().await.map_err(|e| e.to_string())?;

    let id_img_re = regex::Regex::new(
        r#"sharedfiles/filedetails/\?id=(\d+)"[^>]*>\s*<img src="([^"]+)""#,
    )
    .map_err(|e| e.to_string())?;
    let title_re =
        regex::Regex::new(r#"sharedfiles/filedetails/\?id=(\d+)">([^<]+)</a>"#)
            .map_err(|e| e.to_string())?;

    let mut img_by_id: HashMap<String, String> = HashMap::new();
    for cap in id_img_re.captures_iter(&html) {
        img_by_id.insert(cap[1].to_string(), cap[2].to_string());
    }
    let mut title_by_id: HashMap<String, String> = HashMap::new();
    for cap in title_re.captures_iter(&html) {
        title_by_id.insert(cap[1].to_string(), decode_entities(&cap[2]));
    }

    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for cap in id_img_re.captures_iter(&html) {
        let id = cap[1].to_string();
        if !seen.insert(id.clone()) {
            continue;
        }
        items.push(BrowseRawItem {
            id: id.clone(),
            title: title_by_id.get(&id).cloned().unwrap_or_else(|| "未命名".into()),
            preview_url: img_by_id.get(&id).cloned().unwrap_or_default(),
        });
    }

    // 总数：window.SSR 内嵌状态 total_count（多层转义形如 total_count\\\":33952）
    let total = regex::Regex::new(r#"total_count\\*"\s*:\s*(\d+)"#)
        .map_err(|e| e.to_string())?
        .captures(&html)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<usize>().ok())
        .unwrap_or(0);

    Ok(BrowseRawResult {
        has_more: items.len() >= 30,
        total,
        items,
    })
}
