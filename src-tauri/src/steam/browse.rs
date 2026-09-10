//! 工坊浏览页抓取（SSR HTML 解析，对齐 Web 版 providers/browse.ts）
//!
//! Steam 2025 改版后 format=json 已移除，新 UI 条目卡片服务端渲染在 HTML 中：
//! - 锚点 `sharedfiles/filedetails/?id=<id>` 抓 id/预览图/标题（锚点比数据块稳定）
//! - 总数从内嵌 SSR 状态提取：`total_count\" : <n>`（多层转义，宽松正则）

use super::SteamClient;
use crate::util::{decode_entities, url_encode};
use std::collections::{HashMap, HashSet};

pub const BROWSE_URL: &str = "https://steamcommunity.com/workshop/browse/";

/// Steam 工坊 SSR 页固定每页 30 条（`numperpage` 实测被忽略，传什么都返回 30）
pub const PAGE_SIZE: usize = 30;

#[derive(Default, Debug, Clone)]
pub struct BrowseQuery {
    pub query: Option<String>,
    pub sort: Option<String>,
    pub page: Option<u32>,
    /// 必需标签。**Steam 对多个 requiredtags[] 取严格 AND，组内也不例外**
    /// （实测 Anime+Girls 只剩 4 条），所以这里不做任何组内 OR 的假设。
    pub required_tags: Vec<String>,
    /// 排除标签。多个之间是 OR 排除（命中任一即排除）
    pub excluded_tags: Vec<String>,
    /// 趋势时间范围（天）。**实测只对 browsesort=trend 生效**，其他排序下无任何效果
    pub days: Option<u32>,
    /// 创建时间范围（unix 秒）
    pub created_after: Option<i64>,
    pub created_before: Option<i64>,
    /// 更新时间范围（unix 秒）
    pub updated_after: Option<i64>,
    pub updated_before: Option<i64>,
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

/// 拼装工坊浏览 URL。抽出来是为了能离线断言参数格式（见本文件测试）。
pub fn build_browse_url(q: &BrowseQuery) -> String {
    // 只传 browsesort：实测 actualsort 完全无效（传 toprated 仍返回 trend 序），
    // 一并发送只是徒增噪音。
    let sort = q.sort.as_deref().unwrap_or("trend");
    let mut url = format!(
        "{BROWSE_URL}?appid=431960&section=readytouseitems&browsesort={}&p={}",
        url_encode(sort),
        q.page.unwrap_or(1)
    );
    if let Some(query) = &q.query {
        if !query.is_empty() {
            url += &format!("&searchtext={}", url_encode(query));
        }
    }
    // 键名里的 [] 原样发送即可（Steam 对 `requiredtags[]` 与 `requiredtags%5B%5D` 同等对待），
    // 值必须编码 —— 标签名含空格（"Pixel art"、"1920 x 1080"）。
    for t in &q.required_tags {
        url += &format!("&requiredtags[]={}", url_encode(t));
    }
    for t in &q.excluded_tags {
        url += &format!("&excludedtags[]={}", url_encode(t));
    }
    // days 只在趋势排序下有意义，其余排序传了也不起作用，索性不发
    if let Some(d) = q.days {
        if sort == "trend" {
            url += &format!("&days={d}");
        }
    }
    if let Some(v) = q.created_after {
        url += &format!("&created_date_range_filter_start={v}");
    }
    if let Some(v) = q.created_before {
        url += &format!("&created_date_range_filter_end={v}");
    }
    if let Some(v) = q.updated_after {
        url += &format!("&updated_date_range_filter_start={v}");
    }
    if let Some(v) = q.updated_before {
        url += &format!("&updated_date_range_filter_end={v}");
    }
    url
}

pub async fn browse_workshop_raw(
    client: &SteamClient,
    q: &BrowseQuery,
) -> Result<BrowseRawResult, String> {
    let url = build_browse_url(q);

    let resp = client.get(&url).await?;
    let html = resp.text().await.map_err(|e| e.to_string())?;

    let id_img_re =
        regex::Regex::new(r#"sharedfiles/filedetails/\?id=(\d+)"[^>]*>\s*<img src="([^"]+)""#)
            .map_err(|e| e.to_string())?;
    let title_re = regex::Regex::new(r#"sharedfiles/filedetails/\?id=(\d+)">([^<]+)</a>"#)
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
            title: title_by_id
                .get(&id)
                .cloned()
                .unwrap_or_else(|| "未命名".into()),
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
        has_more: items.len() >= PAGE_SIZE,
        total,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q() -> BrowseQuery {
        BrowseQuery::default()
    }

    #[test]
    fn defaults_match_we_official() {
        let url = build_browse_url(&q());
        assert!(url.contains("appid=431960"));
        assert!(url.contains("section=readytouseitems"));
        assert!(url.contains("browsesort=trend"), "默认排序应为 trend");
        assert!(url.contains("p=1"));
        // 严格复刻 WE：默认不预选任何标签，也不排除成人内容
        assert!(!url.contains("requiredtags"), "默认不应带任何标签");
        assert!(!url.contains("excludedtags"), "WE 默认不排除成人内容");
        // actualsort 实测无效，不应再发送
        assert!(!url.contains("actualsort"));
    }

    #[test]
    fn multiple_tags_are_repeated_params_not_comma_joined() {
        let url = build_browse_url(&BrowseQuery {
            required_tags: vec!["Scene".into(), "Anime".into()],
            ..q()
        });
        // 逗号拼接会被 Steam 判为零结果，必须重复参数
        assert!(url.contains("requiredtags[]=Scene"));
        assert!(url.contains("requiredtags[]=Anime"));
        assert!(!url.contains("Scene%2CAnime") && !url.contains("Scene,Anime"));
    }

    #[test]
    fn tag_values_with_spaces_are_encoded() {
        let url = build_browse_url(&BrowseQuery {
            required_tags: vec!["1920 x 1080".into(), "Pixel art".into()],
            ..q()
        });
        assert!(url.contains("requiredtags[]=1920%20x%201080"));
        assert!(url.contains("requiredtags[]=Pixel%20art"));
    }

    #[test]
    fn excluded_tags_emitted() {
        let url = build_browse_url(&BrowseQuery {
            excluded_tags: vec!["Mature".into(), "Questionable".into()],
            ..q()
        });
        assert!(url.contains("excludedtags[]=Mature"));
        assert!(url.contains("excludedtags[]=Questionable"));
    }

    #[test]
    fn days_only_applies_to_trend_sort() {
        let with_trend = build_browse_url(&BrowseQuery {
            sort: Some("trend".into()),
            days: Some(7),
            ..q()
        });
        assert!(with_trend.contains("days=7"));
        // 实测 days 对其他排序无效，不发送以免误导
        let with_recent = build_browse_url(&BrowseQuery {
            sort: Some("mostrecent".into()),
            days: Some(7),
            ..q()
        });
        assert!(!with_recent.contains("days="));
    }

    #[test]
    fn date_ranges_emitted() {
        let url = build_browse_url(&BrowseQuery {
            created_after: Some(1704067200),
            created_before: Some(1706745600),
            updated_after: Some(1704067200),
            ..q()
        });
        assert!(url.contains("created_date_range_filter_start=1704067200"));
        assert!(url.contains("created_date_range_filter_end=1706745600"));
        assert!(url.contains("updated_date_range_filter_start=1704067200"));
        assert!(!url.contains("updated_date_range_filter_end"));
    }

    #[test]
    fn empty_search_text_is_omitted() {
        let url = build_browse_url(&BrowseQuery {
            query: Some(String::new()),
            ..q()
        });
        assert!(!url.contains("searchtext="));
    }
}
